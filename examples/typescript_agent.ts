#!/usr/bin/env node
/**
 * TypeScript Agent Reference Implementation — StateGraph in a Node.js AI workflow.
 *
 * This shows how a TypeScript agent uses StateGraph for structured state
 * management with full provenance.
 *
 * Setup:
 *   cd bindings/typescript && npm install && npm run build
 *
 * Run:
 *   cd bindings/typescript && node ../../examples/typescript_agent.ts
 */

const { StateGraph } = require("../bindings/typescript/index");

function main() {
  console.log("=== TypeScript Agent Reference Implementation ===\n");

  // ─── 1. Create a repository ──────────────────────────────────
  const sg = new StateGraph(); // in-memory. Use new StateGraph("state.db") for SQLite.
  console.log("✓ Repository initialized\n");

  // ─── 2. Set initial state ────────────────────────────────────
  sg.set("/api/name", "my-service", "Initialize API", undefined, "Checkpoint");
  sg.setJson(
    "/api/config",
    {
      port: 3000,
      cors: true,
      rateLimit: { requests: 100, window: "1m" },
      auth: { provider: "jwt", secret: "change-me" },
    },
    "Set API configuration",
    undefined,
    "Checkpoint",
    "agent/setup",
    "Standard API config with JWT auth and rate limiting"
  );
  console.log("✓ Initial state set\n");

  // ─── 3. Branch for feature work ──────────────────────────────
  sg.branch("feature/oauth");
  sg.setJson(
    "/api/config/auth",
    { provider: "oauth2", clientId: "app-123", scopes: ["read", "write"] },
    "Switch to OAuth2 authentication",
    "feature/oauth",
    "Explore",
    "agent/auth",
    "OAuth2 provides better security than JWT for multi-tenant apps",
    0.8
  );
  console.log("✓ Feature branch 'oauth' created\n");

  // ─── 4. Diff branches ───────────────────────────────────────
  const diff = sg.diff("main", "feature/oauth");
  console.log(`Diff main → feature/oauth (${diff.length} changes):`);
  diff.forEach((d: any) => console.log(`  ${d.op}: ${d.path}`));
  console.log();

  // ─── 5. Speculation: explore rate limit approaches ───────────
  console.log("--- Speculation: rate limit approaches ---\n");

  const specStrict = sg.speculate(undefined, "Strict limits");
  const specRelaxed = sg.speculate(undefined, "Relaxed limits");

  sg.specSet(specStrict, "/api/config/rateLimit/requests", 50);
  sg.specSet(specRelaxed, "/api/config/rateLimit/requests", 500);

  console.log(`  Strict: ${sg.specGet(specStrict, "/api/config/rateLimit/requests")} req/min`);
  console.log(`  Relaxed: ${sg.specGet(specRelaxed, "/api/config/rateLimit/requests")} req/min`);

  sg.commitSpeculation(
    specRelaxed,
    "Adopted relaxed rate limits",
    "Refine",
    "API is internal-only, strict limits cause timeout cascades",
    0.9
  );
  sg.discardSpeculation(specStrict);
  console.log("  ✓ Committed relaxed, discarded strict\n");

  // ─── 6. Merge OAuth feature ──────────────────────────────────
  sg.merge("feature/oauth", undefined, "Adopt OAuth2 authentication");
  console.log("✓ Merged OAuth2 into main\n");

  // ─── 7. Query ────────────────────────────────────────────────
  console.log("--- Query: all Explore intents ---\n");
  const explores = sg.query(undefined, undefined, "Explore");
  explores.forEach((e: any) => {
    console.log(`  [${e.intent.category}] ${e.intent.description}`);
    if (e.reasoning) console.log(`    reasoning: ${e.reasoning}`);
  });
  console.log();

  // ─── 8. Blame ────────────────────────────────────────────────
  console.log("--- Blame ---\n");
  const blame = sg.blame("/api/config/rateLimit/requests");
  console.log(`  Who set the rate limit?`);
  console.log(`    agent: ${blame.agent_id}`);
  console.log(`    why: ${blame.intent_description}`);
  if (blame.reasoning) console.log(`    reasoning: ${blame.reasoning}`);
  console.log();

  // ─── 9. Epochs ───────────────────────────────────────────────
  sg.createEpoch("2026-Q2-api-setup", "API service initial setup", ["intent-api"]);
  sg.sealEpoch("2026-Q2-api-setup", "API configured with OAuth2 and relaxed rate limits");
  console.log("✓ Epoch sealed\n");

  const epochs = sg.listEpochs();
  epochs.forEach((e: any) => {
    console.log(`  ${e.id} [${e.status}] — ${e.description}`);
  });
  console.log();

  // ─── 10. Full audit trail ────────────────────────────────────
  console.log("--- Commit log ---\n");
  const log = sg.log();
  [...log].reverse().forEach((c: any) => {
    console.log(`  ${c.id} [${c.intent.category}] ${c.intent.description}`);
    if (c.reasoning) {
      const short = c.reasoning.length > 60 ? c.reasoning.slice(0, 60) + "..." : c.reasoning;
      console.log(`    → ${short}`);
    }
  });

  console.log(`\n=== Complete! ${log.length} commits with full provenance ===`);
}

main();
