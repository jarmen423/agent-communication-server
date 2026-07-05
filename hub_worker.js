#!/usr/bin/env node
/**
 * nats-hub universal worker — single entrypoint for all backend types.
 *
 * Backends:
 *   --type agy|hermes|cursor|cline
 *
 * Examples:
 *   node hub_worker.js --type hermes --identity hermes-1
 *   node hub_worker.js --type cline --identity cline-1 --model "cline-pass/minimax-m3"
 *   node hub_worker.js --type cursor --identity cursor-1
 */

import { connect, JSONCodec, StringCodec } from "nats";
import { readFileSync } from "fs";
import { config } from "dotenv";
import { Agent } from "@cline/sdk";

config({ path: new URL(".env", import.meta.url).pathname });

const sc = StringCodec();
const jc = JSONCodec();

function getCliApiKey() {
  const raw = process.env.CLINE_API_KEY || process.env.CURSOR_API_KEY || process.env.CURSOR_CLOUD_API_KEY;
  if (!raw) throw new Error("CLINE_API_KEY / CURSOR_API_KEY not set");
  const keys = raw.replace(/"/g, "").split(",").map((k) => k.trim()).filter(Boolean);
  if (!keys.length) throw new Error("No valid API keys found");
  return keys[0];
}

async function resolveCliBackend(type, args) {
  if (type === "cline" || type === "cursor") {
    return {
      label: type,
      createAgent: () => new Agent({
        providerId: "cline-pass",
        modelId: args.model,
        apiKey: getCliApiKey(),
        maxIterations: args.maxIterations || 50,
      }),
      collectAssistantOutput(event) {
        if (event.type === "assistant-text-delta" && event.text) return event.text;
        return "";
      },
      textFrom(result) {
        if (typeof result?.text === "string" && result.text) return result.text;
        return JSON.stringify(result);
      },
    };
  }
  throw new Error(`Unsupported CLI backend: ${type}`);
}

const STRATEGIES = {
  agy: undefined,
  hermes: undefined,
  cursor: undefined,
  cline: "cline",
};

async function main() {
  const args = parseArgs();
  if (args.type === "cline" || args.type === "cursor") {
    args.model = args.model || "cline-pass/minimax-m3";
  }

  console.log(`[hub-worker] type=${args.type} identity=${args.identity} model=${args.model || "-"}`);

  const nc = await connect({ servers: args.natsUrl });
  console.log("[hub-worker] connected to NATS");

  const inboxSubject = `channel.inbox.${args.identity}`;
  const sub = nc.subscribe(inboxSubject);
  console.log(`[hub-worker] subscribed to ${inboxSubject}`);

  let broadcastSub = null;
  if (args.channel) {
    broadcastSub = nc.subscribe(`channel.${args.channel}`);
    console.log(`[hub-worker] also subscribed to channel.${args.channel}`);
  }

  setInterval(async () => {
    const heartbeat = {
      meta: {
        id: crypto.randomUUID(),
        from: args.identity,
        channel: "hub.presence",
        kind: "status",
        timestamp: new Date().toISOString(),
      },
      payload: { identity: args.identity },
    };
    nc.publish("hub.presence", jc.encode(heartbeat));
  }, 30000);

  const strategy = STRATEGIES[args.type];
  const backend = strategy === "cline" ? await resolveCliBackend(args.type, args) : null;

  const processMessage = async (msg) => {
    let envelope;
    try {
      envelope = jc.decode(msg.data);
    } catch (e) {
      console.error("[hub-worker] failed to decode envelope:", e.message);
      return;
    }

    const prompt = envelope.payload?.prompt || envelope.payload?.text || envelope.payload?.command;
    if (!prompt) return;

    const taskChannel = envelope.payload?.task_channel || envelope.meta.reply_to || null;
    const statusChannel = taskChannel || envelope.meta.channel;
    publishStatus(nc, args.identity, statusChannel, "working", envelope.meta.from);

    try {
      let resultText;
      if (strategy === "cline") {
        const agent = backend.createAgent();
        let output = "";
        agent.subscribe((event) => {
          output += backend.collectAssistantOutput(event);
        });
        const result = await agent.run(prompt);
        resultText = output.trim() || backend.textFrom(result);
      } else {
        throw new Error(`Unsupported type in this build: ${args.type}`);
      }

      const resultPayload = { result: resultText, task_id: envelope.meta.id, status: "done" };
      if (taskChannel) publishToChannel(nc, args.identity, taskChannel, resultPayload, envelope.meta.id);
      publishReply(nc, args.identity, envelope, resultPayload);
      publishStatus(nc, args.identity, statusChannel, "done", envelope.meta.from);
    } catch (err) {
      console.error(`[hub-worker] task failed:`, err.message);
      const errorPayload = { error: err.message, task_id: envelope.meta.id, status: "error" };
      if (taskChannel) publishToChannel(nc, args.identity, taskChannel, errorPayload, envelope.meta.id);
      publishReply(nc, args.identity, envelope, errorPayload);
      publishStatus(nc, args.identity, statusChannel, "error", envelope.meta.from);
    }
  };

  (async () => {
    for await (const msg of sub) await processMessage(msg);
  })();
  if (broadcastSub) {
    (async () => {
      for await (const msg of broadcastSub) await processMessage(msg);
    })();
  }

  console.log("[hub-worker] ready, waiting for tasks...");
  await nc.closed();
}

function publishReply(nc, fromIdentity, originalEnvelope, payload) {
  const reply = {
    meta: {
      id: crypto.randomUUID(),
      from: fromIdentity,
      to: originalEnvelope.meta.from,
      channel: originalEnvelope.meta.channel,
      kind: "message",
      timestamp: new Date().toISOString(),
      reply_to: originalEnvelope.meta.id,
    },
    payload,
  };
  nc.publish(`hub.send.${originalEnvelope.meta.channel}`, jc.encode(reply));
}

function publishToChannel(nc, fromIdentity, channel, payload, replyTo) {
  const env = {
    meta: {
      id: crypto.randomUUID(),
      from: fromIdentity,
      channel,
      kind: "message",
      timestamp: new Date().toISOString(),
      reply_to: replyTo,
    },
    payload,
  };
  nc.publish(`hub.send.${channel}`, jc.encode(env));
}

function publishStatus(nc, fromIdentity, channel, status, to) {
  const env = {
    meta: {
      id: crypto.randomUUID(),
      from: fromIdentity,
      to: to || undefined,
      channel,
      kind: "status",
      timestamp: new Date().toISOString(),
    },
    payload: { status },
  };
  nc.publish(`hub.send.${channel}`, jc.encode(env));
}

function parseArgs() {
  const args = {
    type: process.argv[2] === "--type" ? process.argv[3] : "cline",
    identity: "worker-1",
    model: null,
    natsUrl: "nats://127.0.0.1:4222",
    channel: null,
    maxIterations: 50,
    systemPrompt: null,
  };

  const argv = process.argv.slice(args.type === process.argv[3] ? 4 : 2);
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case "--type":
        args.type = argv[++i];
        break;
      case "--identity":
        args.identity = argv[++i];
        break;
      case "--model":
        args.model = argv[++i];
        break;
      case "--nats-url":
        args.natsUrl = argv[++i];
        break;
      case "--channel":
        args.channel = argv[++i];
        break;
      case "--max-iterations":
        args.maxIterations = parseInt(argv[++i]);
        break;
      case "--system-prompt":
        args.systemPrompt = argv[++i];
        break;
      case "--help":
        console.log("Usage: node hub_worker.js --type <cline|agy|hermes|cursor> --identity <name> [--model <model>]");
        process.exit(0);
    }
  }
  return args;
}

main().catch((err) => {
  console.error("[hub-worker] fatal:", err);
  process.exit(1);
});