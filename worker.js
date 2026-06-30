#!/usr/bin/env node
/**
 * nats-hub Cline worker — subscribes to a NATS inbox channel, dispatches
 * tasks to a Cline SDK agent, and publishes results back via NATS.
 *
 * This is the dogfooding worker: it uses nats-hub's own messaging bus
 * to receive tasks and a Cline Pass agent (via @cline/sdk) to execute them.
 *
 * Usage:
 *   node worker.js --identity worker-1 --channel agents.tasks --model "cline-pass/minimax-m3"
 *
 * Requires:
 *   - NATS server running on nats://127.0.0.1:4222
 *   - hub-server (control plane router) running
 *   - CLINE_API_KEY in .env or environment
 */

import { Agent } from "@cline/sdk";
import { connect, JSONCodec, StringCodec } from "nats";
import { readFileSync } from "fs";
import { config } from "dotenv";

// Load .env from repo root
config({ path: new URL(".env", import.meta.url).pathname });

const sc = StringCodec();
const jc = JSONCodec();

// Parse CLINE_API_KEY — format: "key1","key2" (pick first)
function getApiKey() {
  const raw = process.env.CLINE_API_KEY;
  if (!raw) throw new Error("CLINE_API_KEY not set in .env or environment");
  // Strip quotes, split by comma, take first
  const keys = raw.replace(/"/g, "").split(",").map(k => k.trim()).filter(Boolean);
  if (keys.length === 0) throw new Error("No valid API keys found");
  return keys[0];
}

async function main() {
  const args = parseArgs();
  const apiKey = getApiKey();

  console.log(`[worker] identity=${args.identity} model=${args.model} nats=${args.natsUrl}`);

  // Connect to NATS
  const nc = await connect({ servers: args.natsUrl });
  console.log(`[worker] connected to NATS`);

  // Subscribe to our inbox (DM channel)
  const inboxSubject = `channel.inbox.${args.identity}`;
  const sub = nc.subscribe(inboxSubject);
  console.log(`[worker] subscribed to ${inboxSubject}`);

  // Optionally subscribe to a broadcast channel
  let broadcastSub = null;
  if (args.channel) {
    broadcastSub = nc.subscribe(`channel.${args.channel}`);
    console.log(`[worker] also subscribed to channel.${args.channel}`);
  }

  // Heartbeat loop
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

  // Process messages from inbox and broadcast
  const processMessage = async (msg) => {
    let envelope;
    try {
      envelope = jc.decode(msg.data);
    } catch (e) {
      console.error("[worker] failed to decode envelope:", e.message);
      return;
    }

    const prompt = envelope.payload?.prompt || envelope.payload?.text || envelope.payload?.command;
    if (!prompt) {
      console.log("[worker] no prompt found in payload, skipping");
      return;
    }

    console.log(`[worker] received task from ${envelope.meta.from}: ${prompt.slice(0, 80)}...`);

    // Publish status: working
    publishStatus(nc, args.identity, envelope.meta.channel, "working", envelope.meta.from);

    try {
      // Create Cline agent and run
      const agent = new Agent({
        providerId: "cline-pass",
        modelId: args.model,
        apiKey: apiKey,
        maxIterations: args.maxIterations || 50,
        systemPrompt: args.systemPrompt || "You are a helpful coding assistant. Complete the task and provide a clear summary of what you did.",
      });

      // Collect output
      let output = "";
      agent.subscribe((event) => {
        if (event.type === "assistant-text-delta" && event.text) {
          output += event.text;
        }
      });

      const result = await agent.run(prompt);

      // Build result text
      let resultText = output.trim();
      if (!resultText && result?.text) resultText = result.text;
      if (!resultText) resultText = JSON.stringify(result);

      console.log(`[worker] task completed (${resultText.length} chars)`);

      // Publish result back to sender's inbox
      publishReply(nc, args.identity, envelope, {
        result: resultText,
        task_id: envelope.meta.id,
        status: "done",
      });

      // Publish status: done
      publishStatus(nc, args.identity, envelope.meta.channel, "done", envelope.meta.from);

    } catch (err) {
      console.error(`[worker] task failed:`, err.message);

      publishReply(nc, args.identity, envelope, {
        error: err.message,
        task_id: envelope.meta.id,
        status: "error",
      });

      publishStatus(nc, args.identity, envelope.meta.channel, "error", envelope.meta.from);
    }
  };

  // Process inbox messages
  (async () => {
    for await (const msg of sub) {
      await processMessage(msg);
    }
  })();

  // Process broadcast messages (if subscribed)
  if (broadcastSub) {
    (async () => {
      for await (const msg of broadcastSub) {
        await processMessage(msg);
      }
    })();
  }

  console.log(`[worker] ready, waiting for tasks...`);

  // Keep alive
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
  // Publish to hub.send (router will route to sender's inbox via meta.to)
  nc.publish(`hub.send.${originalEnvelope.meta.channel}`, jc.encode(reply));
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
    identity: "cline-worker",
    model: "cline-pass/minimax-m3",
    natsUrl: "nats://127.0.0.1:4222",
    channel: null,
    maxIterations: 50,
    systemPrompt: null,
  };

  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i++) {
    switch (argv[i]) {
      case "--identity": args.identity = argv[++i]; break;
      case "--model": args.model = argv[++i]; break;
      case "--nats-url": args.natsUrl = argv[++i]; break;
      case "--channel": args.channel = argv[++i]; break;
      case "--max-iterations": args.maxIterations = parseInt(argv[++i]); break;
      case "--system-prompt": args.systemPrompt = argv[++i]; break;
      case "--help":
        console.log("Usage: node worker.js --identity <name> --model <model> [--channel <ch>] [--nats-url <url>]");
        process.exit(0);
    }
  }

  return args;
}

main().catch(err => {
  console.error("[worker] fatal:", err);
  process.exit(1);
});