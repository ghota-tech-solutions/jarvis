-- § T2.1 — five-tier memory layers.
--
-- Adds a `layer` column to the memories table so the prompt-assembly
-- layer can prioritise procedural / semantic memories over episodic
-- noise, and so archival entries can be skipped without losing them
-- from the ledger. Defaults to 'semantic' (the implicit pre-T2.1
-- behaviour) so existing rows migrate without re-tagging.
--
-- Valid values: 'working' | 'episodic' | 'semantic' | 'procedural'
-- | 'archival'. The constraint is enforced application-side (the Rust
-- `MemoryLayer` enum) rather than via CHECK to keep migration friendly
-- on older SQLite versions.

ALTER TABLE memories ADD COLUMN layer TEXT NOT NULL DEFAULT 'semantic';

-- New composite index for the prompt-assembly query that filters by
-- scope+status+layer. The existing `idx_memories_scope_status`
-- (from 0002) still serves the SPA's listing path.
CREATE INDEX IF NOT EXISTS idx_memories_layer_status
    ON memories (layer, status);
