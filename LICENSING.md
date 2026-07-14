# Licensing

AgentStateGraph is licensed under the **Business Source License 1.1
(BSL 1.1)**. See [LICENSE](LICENSE) for the authoritative license
text. This document is a plain-English summary of what that license
means.

## What this means for you

### You CAN (without any commercial license):

- Use AgentStateGraph in production for your own applications and
  services
- Use it inside your company, startup, or enterprise for internal
  operations — employees, contractors, and subsidiaries all count as
  internal
- Self-host AgentStateGraph on your own infrastructure
- Modify the source code and create derivative works for your own
  internal use
- Build applications and services that use AgentStateGraph
  internally as infrastructure — your product or service can depend
  on an ASG instance you run
- Use the MCP server, language bindings, CLI, and all features as
  part of your own business operations
- Use it for research, education, testing, and development

### You CANNOT (without a commercial license):

- **Offer AgentStateGraph itself as a commercial managed service** —
  e.g., "AgentStateGraph-as-a-Service" where the primary value is
  access to ASG's features
- **Embed, bundle, distribute, or sublicense AgentStateGraph as
  part of a product or service you sell, license, or distribute to
  third parties** — e.g., shipping ASG inside your own commercial
  agent framework or platform that customers buy

## The restrictions, explained

BSL 1.1 with this Additional Use Grant protects AgentStateGraph
against two specific patterns that would undermine the project's
sustainability:

**1. Hosted resale.** A cloud provider taking the code, hosting it,
and selling "Managed AgentStateGraph" as a commercial service. This
is what BSL was originally designed to prevent — it's the pattern
that led to MongoDB, Elastic, and Redis changing their licenses
after hyperscalers strip-mined their ecosystems.

**2. Redistribution for sale.** A software vendor embedding
AgentStateGraph into a product they distribute to customers,
effectively getting commercial value from the substrate without any
licensing arrangement. This is the "we ship ASG inside our agent
platform and charge our customers for it" pattern. Under the
Additional Use Grant, this requires a commercial license.

If you're building an application that uses AgentStateGraph
internally as infrastructure — your team runs ASG, your application
connects to it, and you're not redistributing or reselling ASG
itself — you're fine. That's internal business use, which is fully
permitted.

If you want to embed AgentStateGraph into a product you distribute
to third parties, resell it, or run a hosted ASG service for
customers, you need a commercial license. Contact us at
**license@agentstatelabs.com**.

## Automatic conversion to Apache 2.0

Every version of AgentStateGraph automatically converts to the
**Apache License 2.0** four years after its release date:

- **v0.9.2** (published 2026-07-13) becomes Apache 2.0 on
  **2030-07-13**
- Future versions follow the same rolling per-version pattern

After conversion, all BSL restrictions lift for that version — it
becomes permissively licensed Apache 2.0. You can embed it, resell
it, ship it in your products, host it as a managed service. The
four-year clock is what keeps the ecosystem protected while
guaranteeing long-term openness.

## Why BSL?

AgentStateGraph is the substrate for agent state — a new
infrastructure primitive. Building and maintaining it requires
sustained investment in the core engine, storage backends, sibling
crates (tasks, policies, and future primitives), language bindings,
documentation, security patches, and community support. The BSL
model has been proven by CockroachDB, Sentry, HashiCorp, MariaDB,
and others to sustain open-source infrastructure projects while
preventing the well-documented pattern of hyperscale cloud
providers strip-mining open-source value.

We chose BSL 1.1 with a redistribution-restricted Additional Use
Grant specifically because:

- It's the most battle-tested source-available license for
  infrastructure software
- The automatic Apache 2.0 conversion gives the community a
  permanent guarantee
- End users running AgentStateGraph internally are completely
  unaffected
- The redistribution carve-out prevents packaged resale that would
  bypass the commercial licensing we rely on for sustainability
- It's clear, readable, and has well-understood legal precedent

## Previous versions

Versions prior to 0.3.5-beta.2 were released under MIT OR Apache-2.0.
Those releases retain their original license.

## Commercial licensing

If your use case requires terms beyond the BSL 1.1 grant — for
example, you want to embed AgentStateGraph into a product you
distribute, or offer a hosted ASG service to customers — contact us
at **license@agentstatelabs.com** for commercial licensing options.

We offer two commercial tiers:

- **ASG Enterprise** — for ISVs and AI product companies embedding
  AgentStateGraph as the substrate of their own commercial products
- **ASG redistribution license** — for packaged resale and bundled
  distribution scenarios

## Questions

If you're unsure whether your use case is covered by the BSL 1.1
grant, email **license@agentstatelabs.com** and we'll clarify.
The bar is straightforward: internal use is free; redistribution or
hosted resale requires a commercial license.

Contact: **license@agentstatelabs.com**
