# Projects as a business hub

Signed-off prototype: `~/tmp/tab-proto/index.html` (5 screens). Ask goes away; Projects is where you run
a business: its queue, a coordinator that watches it, and everything it knows (sites, files, email,
repos, services), searchable by agents within the access level each resource has.

## What exists (Nov 11)

- Projects: `projects` (+ about, rules), `project_notes` (steward-reviewed), `project_repos`, `items.project_id`. All synced.
- Ask: a keyed session `ask:<project|all>` that answers through tab_search / tab_graph_* / tab_project.
- Search: Model2Vec embeddings in sqlite-vec (`doc_vec`), items and notes only, no chunking, local cache (not synced).
- Sessions: Claude Code per turn with `--resume`; each turn reports `meta.context.used`. No compaction.
- Sync: plaintext rows to the hub. No end-to-end encryption, no device keys. `vault.rs` is OS keychain/DPAPI, per device.

## Access levels

Every resource has exactly one:

| Level | Coordinator | Worker threads | Cloud jobs |
|---|---|---|---|
| `all` (site, repos, docs) | yes | yes | yes |
| `coordinator` (billing email, Stripe, contracts) | yes | ask you, per field | no |
| `vault` (EIN, taxes, bank, IDs) | after unlock | ask you, per field | no |

Enforced in tab's MCP server (`call(session, ..)` already knows the session): search filters chunks by
the session's level, which comes from its key (`coordinator:<project>`), never from a tool argument.
Remote MCP services marked `coordinator` are only offered to the coordinator session.

## Phases

1. **Projects replaces Ask (this PR).** Ask tab and its commands removed; graph moves out of the nav.
   Project page with Home / Queue / Knowledge / Notes / Settings. Home = project queue | coordinator chat |
   instructions + resources. Coordinator is a keyed session `coordinator:<project_id>` with a brief that
   makes it the project's watcher. New synced table `project_resources` (kind, url/ref, title, access,
   options, status); repos show alongside. Add-resource dialog recognises links (GitHub, Cloudflare,
   Stripe, Google Drive/Docs, Slack, Notion, Linear, any website) and email labels/senders, and files.
2. **Index resources (RAG).** Chunker + `doc:res:<id>:<n>` in `doc_vec`, with `access` and `project_id`
   columns on the chunk table. Fetchers: website crawl (depth, schedule) through the gateway, files from
   the hub blob store (PDF/text/markdown/docx/xlsx text), repo docs from the checkout or GitHub, email by
   label/sender through the email connector. `tab_search` returns resource chunks with citations, filtered
   by the session's level. Status and chunk counts on Knowledge; "last used" from tool calls.
3. **Coordinator.** Wakes on project queue changes and on a schedule (morning brief, hourly check) as a
   steered turn; can start threads, draft replies, file notes; asks before email/merge/deploy/refunds.
   Auto-compaction at 200k: when a turn ends above the threshold, a small call writes the carry-forward
   summary (open threads, decisions, watching, new notes; vault values never included), the old agent
   session is archived as a history segment (still indexed for the coordinator), and the next turn starts
   a fresh `--session-id` with the summary in the brief. Threshold is a project setting.
4. **Vault (end-to-end encrypted).** See below.

## Vault design

Goal: the hub (local or cloud) only ever holds ciphertext; any device you sign in on can read it once
an existing device (or your recovery key) lets it in; indexing happens only on devices.

- **Keys.** A random 256-bit vault key per account. Each device makes an X25519 key pair at first
  launch; the private key stays in the OS keychain/DPAPI (`vault.rs`). The vault key is stored in the
  hub wrapped once per device (`vault_keys`: device id, public key, wrapped key, via HPKE/XChaCha20-Poly1305)
  plus once under a recovery key (shown once, 24 words) and optionally a passkey PRF.
- **Adding a device.** A new device signs in, publishes its public key, and shows a short code. An
  unlocked device approves it (compares the code) and writes a wrapped key for it. Lost every device:
  recovery key. Removing a device: rotate the vault key and rewrap for the remaining ones.
- **Data.** Vault resources are records whose body is `{ciphertext, nonce, key_id}`; routing fields
  (project id, kind, updated_at) stay readable. Files are encrypted before `PUT /files`, so the content
  hash is of ciphertext. The hub, cloud runner and cloud jobs can't read any of it.
- **Index.** Each device decrypts and indexes locally into a separate, encrypted sqlite-vec database
  (`vault.db`, SQLCipher-style page encryption with a key derived from the vault key), never into
  `tab.db`. To save re-indexing on a new device, the built index can be synced as an encrypted blob.
- **Use.** Unlock with Touch ID / Windows Hello for 10 minutes. Only `coordinator:<project>` sessions
  (Person trust, this device) get `tab_vault_search` / `tab_vault_get`; values come back masked unless
  you reveal them. A worker thread calls `tab_vault_request(fields, why)`, which shows an approval card;
  approved values are passed in that one tool result only. Synced transcripts store the masked form;
  compaction summaries drop vault values. Every read goes to an append-only `vault_audit`.

## Open questions

- Which file types phase 2 parses natively vs. through a converter (PDF text via `pdf-extract`?).
- Email resources: by label/sender only (decided), whole inbox later.
