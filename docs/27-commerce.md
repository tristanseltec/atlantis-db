# 27 — Commerce

> **Status:** Strategy and target design (Stages C0–C4). Nothing in this chapter is implemented. Reviewed 2026-11-16.
> **Crates:** `atlantis-core`, `atlantis-engine`, `atlantis-vector`, `atlantis-text` (planned), `atlantis-mcp` (planned), `atlantis-commerce` (planned, commercial)

Atlantis DB's first vertical is commerce, for human shoppers and for agents, in B2B and
B2C. This chapter states why, what is built, and how it is measured. The system design
is in [02 — Architecture § Commerce Layer](02-architecture.md#commerce-layer-planned);
what is free and what is paid is in
[02 — Architecture § Editions](02-architecture.md#editions-and-extension-points-planned).

## Thesis

Every commerce interaction asks one question: *what is the right thing for this buyer,
under their constraints, and is it truly available at this price now?* Humans ask it
through search; agents ask it through tool calls. It maps directly onto transactional
hybrid retrieval ([26](26-positioning-and-targets.md)):

| Commerce need | Atlantis capability |
|---|---|
| "Something like this", from text or an image | Vector search |
| Exact SKU, MPN, GTIN, part number | BM25 and exact lookup |
| Variants, compatibility, substitutes, bundles, account hierarchy | Graph |
| This buyer's slice: contract, region, channel | Filtered search at ≤ 1% selectivity |
| Price and stock true when answered and when ordered | One MVCC snapshot, read-your-writes |
| Cart, quote and reservation in one step | ACID transactions |

For agents the problem is **grounding**: a wrong price or phantom stock costs money and
trust. Atlantis answers from one snapshot of the system of record, with no lag between
a search index and the data.

## Opportunity Map

| | **B2C** | **B2B** |
|---|---|---|
| **Humans** | Semantic + keyword search with facets; visual search; personalization from session memory; bundles | Part-number search with semantic fallback; contract catalogs and prices; reorder; "what fits machine X" |
| **Agents** | Shopping assistants calling catalog and checkout tools; restock and price-drop watches | Procurement agents: RFQs, substitutes, delivery dates against contracts and budgets; sales-rep copilots; quoting agents |

**Lead with B2B entitled retrieval**, served to humans and agents. Filtering at 1%
selectivity is the default in B2B, exact part-number plus semantic plus compatibility
search is hard for search-only products, procurement agents are a credible early
agent workload, and distributors pay for correctness. B2C follows on the same engine
through buyer memory, image vectors and the same agent gateway.

Atlantis is the engine and a commerce product beside the platform of record (Shopify,
commercetools, SAP, an ERP). It is **not** a commerce platform.

## Reference Model

```
(:Product)-[:HAS_VARIANT]->(:Sku {sku, mpn, gtin, attrs, text_vec, image_vec})
(:Sku)-[:FITS]->(:Assembly)            (:Sku)-[:SUBSTITUTES {certified}]->(:Sku)
(:Sku)-[:IN_CATEGORY]->(:Category)     (:Sku)-[:STOCKED_AT {qty, reserved}]->(:Warehouse)
(:Warehouse)-[:SERVES]->(:Region)
(:Account)-[:PARENT_OF]->(:Account)    (:Account)-[:HAS_CONTRACT]->(:Contract)
(:Contract)-[:ENTITLES]->(:Category|:Sku)
(:Contract)-[:PRICES {tiers}]->(:Sku)
(:Buyer)-[:MEMBER_OF]->(:Account)      (:Buyer)-[:REMEMBERS]->(:Memory {text, vec})
(:Quote)-[:FOR]->(:Account)            (:Quote)-[:LINE {qty, unitPrice}]->(:Sku)
```

## Example: One Agent Tool Call

"Stainless M8 bolts that fit pump P-200, deliverable to Hamburg by Friday, at my
contract price."

```cypher
CALL commerce.search($account, 'stainless M8 bolt', $qvec, 20) YIELD sku, score
MATCH (sku)-[:FITS]->(:Assembly {id: 'P-200'})
MATCH (sku)-[s:STOCKED_AT]->(w:Warehouse)-[:SERVES]->(:Region {code: 'DE-HH'})
WHERE s.qty - s.reserved >= $qty AND w.leadDays <= $daysLeft
RETURN sku.sku, sku.title, commerce.price($account, sku, $qty) AS unitPrice,
       w.id, score
ORDER BY score DESC LIMIT 5
```

`commerce.reserve` then runs in the same transaction, so the agent never quotes stock
that disappears before checkout.

## Agent Protocols

MCP serves as the data and tool layer; ACP (OpenAI and Stripe) covers checkout; UCP
(Google, with Shopify and large retailers) covers discovery through cart; AP2 covers
payment authorization. Merchants increasingly report supporting more than one, and the
landscape is unsettled, so Atlantis builds **one tool core** (search, compare,
availability, price, quote, reserve) and thin protocol adapters on top of it.

## Engine Requirements

| Need | Today | Change |
|---|---|---|
| Filtered hybrid search, BM25, fusion, read-your-writes | Focus track F2–F5 | None; commerce validates it |
| `Decimal` for money | Open (Phase 2) | Pulled forward to C1 |
| Facet counts over a filtered result set | Scans at ~4 µs per row | Bitmap facets (C1) |
| Hot stock counters under concurrent checkout | Optimistic MVCC, conflicts on hot rows | Escrow reservations (C2) |
| Several vectors per SKU (text, image) | One indexed property per index | Several indexes per label, fused (C1) |
| Unique SKU / GTIN | Phase 2 | Minimal unique constraints pulled forward (C1) |
| Sync from the platform of record; events out | Phase 4 | Ingest connectors and outbound change feed (C2) |
| Network access for agents | Phase 5 | `atlantis-mcp` with scoped API keys ahead of full auth (C1) |
| 1–20M SKUs | Targets set at 1M vectors | 10M tier in the benchmark |
| Spatial, time-series | Deferred | Stay deferred; region codes and lead days suffice |

## Targets

Measured by a **Commerce scenario** in the Retrieval Bench: 5M SKUs with text and image
vectors, 10,000 accounts with contract entitlements, a compatibility graph, and
concurrent checkouts on a hot-SKU distribution.

| Metric | Target |
|---|---|
| Agent tool call (entitled search + graph + price), p99, embedded | ≤ 15 ms |
| Human search with 6 facets, p99 | ≤ 30 ms |
| Exact part-number / MPN recall | 100% |
| Entitlement leaks (SKU or price outside the contract) | 0, property-tested |
| Oversell, 500 concurrent checkouts on 50 hot SKUs | 0 |
| Price and stock in the answer vs. at commit | Identical (one snapshot) |
| NDCG@10 vs. BM25-only on a labelled B2B query set | +15% |

## Stages

| Stage | Work | Exit |
|---|---|---|
| **C0** (with F1–F5) | Commerce dataset generator, labelled B2B query set, commerce scenario in the harness | Baseline recorded |
| **C1 — read path** | `Decimal`, bitmap facets, multi-vector fusion, unique constraints, reference schema, `commerce.search` / `commerce.price`, read-only `atlantis-mcp` | Search, facet and entitlement targets met |
| **C2 — write path** | Escrow reservations, cart, quote / RFQ, orders; connectors (Shopify, commercetools, generic ERP via change feed) | Zero oversell; quote-to-reserve in one transaction |
| **C3 — agents and memory** | ACP and UCP adapters, buyer memory, live restock and price-drop queries, agent audit trail | An external agent completes discovery → quote → checkout on a demo store |
| **C4 — design partners** | Two B2B distributors, one mid-market B2C merchant | Measured relevance and conversion lift; production pilot |

## Risks

- **Scope creep into a platform.** Integrate with platforms of record; never replace them.
- **Search SaaS incumbents** own merchandising UIs. Win on entitlement-correct,
  transactional, graph-aware, embeddable retrieval.
- **Slow consumer agent adoption.** Human B2B search and quoting must pay on their own.
- **Engine timeline.** C1 depends on F2–F5; the vector core stays the critical path.

## What's Next

- [02 — Architecture § Commerce Layer](02-architecture.md#commerce-layer-planned)
- [02 — Architecture § Editions](02-architecture.md#editions-and-extension-points-planned) — what is open and what is commercial
- [26 — Positioning & Performance Targets](26-positioning-and-targets.md)
