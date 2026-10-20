## **install plugins**
codex
codex plugin marketplace add ./  # from ~/nats
# Then install nats-hub from the local marketplace in the ChatGPT desktop app

claude code:
claude plugin marketplace add jarmen423/agent-communication-server
# Or test with: claude --plugin-dir ./claude-code-plugin

hemes agent:
cp -R ~/nats/hermes-plugin ~/.hermes/plugins/nats-hub
hermes plugins enable nats-hub