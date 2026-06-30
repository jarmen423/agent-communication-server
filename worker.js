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

    // Extract task channel (for hub-delegate pattern) or fall back to sender inbox
    const taskChannel = envelope.payload?.task_channel || envelope.meta.reply_to || null;
    if (taskChannel) {
      console.log(`[worker] task channel: ${taskChannel}`);
    }

    console.log(`[worker] received task from ${envelope.meta.from}: ${prompt.slice(0, 80)}...`);

    // Publish status: working (to task channel if available, otherwise to original channel)
    const statusChannel = taskChannel || envelope.meta.channel;
    publishStatus(nc, args.identity, statusChannel, "working", envelope.meta.from);

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

      // Publish result to task channel (for hub-delegate) AND sender's inbox (for DM pattern)
      const resultPayload = {
        result: resultText,
        task_id: envelope.meta.id,
        status: "done",
      };

      if (taskChannel) {
        // Publish to task channel: channel.task.<uuid>
        publishToChannel(nc, args.identity, taskChannel, resultPayload, envelope.meta.id);
      }
      // Also send reply to sender's inbox (DM pattern)
      publishReply(nc, args.identity, envelope, resultPayload);

      // Publish status: done
      publishStatus(nc, args.identity, statusChannel, "done", envelope.meta.from);

    } catch (err) {
      console.error(`[worker] task failed:`, err.message);

      const errorPayload = {
        error: err.message,
        task_id: envelope.meta.id,
        status: "error",
      };

      if (taskChannel) {
        publishToChannel(nc, args.identity, taskChannel, errorPayload, envelope.meta.id);
      }
      publishReply(nc, args.identity, envelope, errorPayload);
      publishStatus(nc, args.identity, statusChannel, "error", envelope.meta.from);
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

/// Publish a message to a task channel (e.g. channel.task.<uuid>).
/// This is a broadcast on the task channel — hub-delegate is subscribed there.
/// Do NOT set meta.to — if set, the router would route it as a DM instead
/// of broadcasting to channel.task.<uuid>.
function publishToChannel(nc, fromIdentity, channel, payload, replyTo) {
  const env = {
    meta: {
      id: crypto.randomUUID(),
      from: fromIdentity,
      // No meta.to — broadcast on the task channel
      channel,
      kind: "message",
      timestamp: new Date().toISOString(),
      reply_to: replyTo,
    },
    payload,
  };
  // Broadcast on the task channel (no meta.to = router broadcasts to channel.<name>)
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