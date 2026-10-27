-- case — one firmware, two cold XDP images, tcpdump text.
-- Loaded after formal.sql (needs exam).

CREATE TABLE IF NOT EXISTS case_obs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL REFERENCES exam (id) ON DELETE CASCADE,
    seq INT NOT NULL CHECK (seq >= 1),
    xdp TEXT NOT NULL,
    capability TEXT NOT NULL,
    action TEXT NOT NULL,
    expect TEXT NOT NULL,
    map_name TEXT NOT NULL,
    map_slot INT NOT NULL,
    map_before BIGINT NOT NULL,
    map_after BIGINT NOT NULL,
    sent INT NOT NULL,
    saw_reply BOOLEAN NOT NULL,
    ok BOOLEAN NOT NULL,
    tcpdump TEXT NOT NULL,
    UNIQUE (exam_id, seq)
);

CREATE INDEX IF NOT EXISTS case_obs_exam ON case_obs (exam_id);
