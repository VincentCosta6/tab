# Projects as a business hub

Signed-off prototype: `~/tmp/tab-proto/index.html` (5 screens). Ask goes away; Projects is where you run
a business: its queue, a coordinator that watches it, and everything it knows (sites, files, email,
repos, services), searchable by agents within the access level each resource has.

## What exists (Nov 11)

- Projects: `projects` (+ about, rules), `project_notes` (steward-reviewed), `project_repos`, `items.project_id`. All synced.
- Ask: a keyed session `ask:<project|all>` that answers through tab_search / tab_graph_* / tab_project.
- Search: Model2Vec embeddings in sqlite-vec (`doc_vec`), items and notes, local cache (not synced).
- Resources indexed (phase 2): `crate::knowledge` reads websites (gateway, robots.txt, public hosts only),
  hub files (PDF/docx/xlsx/text), email by label/sender and repo docs from the checkout into
  `resource_chunks` (+ FTS5, `doc:res:<id>:<n>` vectors), on each device. `tab_search` returns passages
  under `resources`, filtered by the reader's level from the session key; `resource_index` keeps
  schedule, errors and "last used" per device.
- Sessions: Claude Code per turn with `--resume`; each turn reports `meta.context.used`.
- Coordinator (phase 3, `agents/coordinator.rs`): a monitor on the device that runs the chat wakes it
  with a turn of tab's own (a `wake` note in the chat) on settled queue changes (2 min quiet, 10 min
  max), hourly while something urgent or a thread waits, and on weekdays at the brief time. When a turn
  ends above `projects.compact_at`, a small-model call writes the carry-forward summary, the stretch
  becomes a synced `session_segments` row (old agent session id, editable summary), a `compacted`
  divider is posted and the next turn starts a fresh session with the summary in its system prompt.
  Segment transcripts are indexed into `resource_chunks` as coordinator-only "Coordinator history".
- Prompt injection: the coordinator reads outside content with more access than other chats. Its
  brief says outside content is data; wake prompts, summarizer input and the carried-forward summary
  are fenced (`coordinator::fenced`); shell commands and WebFetch always ask; remote tools named like
  send/merge/deploy/refund/post/share/delete ask even when allowed elsewhere; on turns tab started
  itself, WebSearch and starting an agent ask too.
- Sync: plaintext rows to the hub, except the vault (phase 4, `crate::e2e`), which syncs only public keys,
  wrapped keys, MACs and ciphertext. `vault.rs` is the OS keychain/DPAPI per device; it holds each
  device's vault private key.

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
4. **Vault (end-to-end encrypted).** See below. Knowledge → Vault: set up (recovery key shown once),
   unlock for 10 minutes, entries with masked fields, devices (approve by code, remove = new key), activity.

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

### As built (phase 4)

- **Tables** (all synced): `vault_devices` (public key, pending/active/removed, a `vouch` MAC under the
  vault key over id + public key, so rotation never wraps for a key the hub swapped in),
  `vault_generations` (each new key carries the previous one sealed under it and is endorsed by a MAC
  under the previous one), `vault_keys` (one wrap per device per generation; X25519 ephemeral-static +
  HKDF-SHA256 + XChaCha20-Poly1305, bound to key id and recipient), `vault_audit` (append only: update
  and delete triggers ignore, so neither this device nor the hub can rewrite it; details sealed).
  `vault_trust` (not synced) keeps fingerprints of generations this device opened: a newer one must
  chain back to one of them, so a key the hub made up is passed over.
- **Recovery key**: 24 BIP-39 words derive an X25519 key pair, so "recovery" is wrapped for like a
  device and rewrapped on rotation without the words. No passkey PRF yet.
- **Entries** are `project_resources` rows with `access = 'vault'`, kind `note` (fields + note) or
  `file`, title "Vault entry"; the real body is sealed in `options.sealed`, bound to project, id and
  kind. `resource_save` still refuses `vault` (nothing plaintext can be saved as one); `crate::e2e`
  writes them with `vault_entry_put`.
- **Index**: in memory while open, saved as `vault-index.sealed` (sealed JSON under a key derived from
  the vault key), not SQLCipher/sqlite-vec: a vault is tens of entries, matched by words. The index
  blob isn't synced yet; a new device re-reads the entries when it first opens the vault.
- **Use**: `tab_vault_search` / `tab_vault_get` are listed only to `coordinator:<project>` chats with
  Person trust running on this device; other Person-trust threads in the project get
  `tab_vault_request(fields, why)`. Unlocking, releasing values to the coordinator and sharing with a
  thread all go through an approval card (`tool = "tab_vault"`); approving a locked vault runs Touch ID /
  Windows Hello first. Released values are remembered for the app's run and masked by
  `e2e::redact` in every stored (and so synced) chat message except this device's tool output, in
  compaction transcripts and in summaries.
- **Rotation** on removing a device reseals every entry and re-uploads every file.

## Open questions

- Phase 2 parses everything natively: `pdf-extract` for PDFs, zip + quick-xml for docx/xlsx. Scanned
  PDFs (no text layer) fail with a message; OCR is later.
- Email resources: by label/sender only (decided), whole inbox later.
