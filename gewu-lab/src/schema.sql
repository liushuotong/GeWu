CREATE TABLE IF NOT EXISTS lab_labs (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    current_interest_revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS lab_interest_revisions (
    lab_id TEXT NOT NULL REFERENCES lab_labs(id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    interest TEXT NOT NULL,
    rationale TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (lab_id, revision)
);
CREATE TRIGGER IF NOT EXISTS lab_interest_no_update
BEFORE UPDATE ON lab_interest_revisions BEGIN
    SELECT RAISE(ABORT, 'interest revisions are immutable');
END;
CREATE TRIGGER IF NOT EXISTS lab_interest_no_delete
BEFORE DELETE ON lab_interest_revisions BEGIN
    SELECT RAISE(ABORT, 'interest revisions are immutable');
END;
CREATE TABLE IF NOT EXISTS lab_projects (
    id TEXT PRIMARY KEY NOT NULL,
    lab_id TEXT NOT NULL REFERENCES lab_labs(id),
    name TEXT NOT NULL,
    question TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed', 'active', 'cancelled')),
    interest_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (lab_id, interest_revision) REFERENCES lab_interest_revisions(lab_id, revision)
);
CREATE TABLE IF NOT EXISTS lab_tasks (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES lab_projects(id),
    objective TEXT NOT NULL,
    context TEXT NOT NULL,
    deliverable TEXT NOT NULL,
    completion_criteria TEXT NOT NULL,
    owner TEXT,
    priority INTEGER NOT NULL,
    priority_override INTEGER,
    status TEXT NOT NULL DEFAULT 'ready' CHECK (status IN ('ready', 'in_progress', 'awaiting_review', 'blocked', 'completed', 'cancelled')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS lab_dependencies (
    task_id TEXT NOT NULL REFERENCES lab_tasks(id),
    depends_on TEXT NOT NULL REFERENCES lab_tasks(id),
    PRIMARY KEY (task_id, depends_on),
    CHECK (task_id != depends_on)
);
CREATE TABLE IF NOT EXISTS lab_attempts (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES lab_tasks(id),
    worker_id TEXT NOT NULL,
    lease_token TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('running', 'submitted', 'accepted', 'rejected', 'failed', 'expired', 'cancelled')),
    claimed_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    completed_at INTEGER,
    output TEXT,
    evidence TEXT,
    uncertainties TEXT,
    accepted INTEGER CHECK (accepted IN (0, 1)),
    feedback TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS lab_one_running_attempt ON lab_attempts(task_id) WHERE status = 'running';
CREATE INDEX IF NOT EXISTS lab_attempt_expiry ON lab_attempts(status, expires_at);
CREATE INDEX IF NOT EXISTS lab_task_queue ON lab_tasks(project_id, status);
CREATE TABLE IF NOT EXISTS lab_decisions (
    id TEXT PRIMARY KEY NOT NULL,
    lab_id TEXT NOT NULL REFERENCES lab_labs(id),
    project_id TEXT REFERENCES lab_projects(id),
    task_id TEXT REFERENCES lab_tasks(id),
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    rationale TEXT NOT NULL,
    priority INTEGER,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS lab_artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    task_id TEXT NOT NULL REFERENCES lab_tasks(id),
    attempt_id TEXT NOT NULL REFERENCES lab_attempts(id),
    title TEXT NOT NULL,
    kind TEXT NOT NULL,
    content TEXT NOT NULL,
    source TEXT,
    provenance TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS lab_artifact_task ON lab_artifacts(task_id, attempt_id);
CREATE TABLE IF NOT EXISTS lab_planning_proposals (
    id TEXT PRIMARY KEY NOT NULL,
    lab_id TEXT NOT NULL REFERENCES lab_labs(id),
    interest_revision INTEGER NOT NULL,
    spec_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('proposed', 'approved', 'rejected')),
    created_at INTEGER NOT NULL,
    FOREIGN KEY (lab_id, interest_revision) REFERENCES lab_interest_revisions(lab_id, revision)
);
CREATE TABLE IF NOT EXISTS lab_proposal_decisions (
    id TEXT PRIMARY KEY NOT NULL,
    proposal_id TEXT NOT NULL REFERENCES lab_planning_proposals(id),
    kind TEXT NOT NULL CHECK (kind IN ('approve', 'reject')),
    actor TEXT NOT NULL,
    rationale TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS lab_planning_proposal_lab ON lab_planning_proposals(lab_id, created_at, id);
CREATE INDEX IF NOT EXISTS lab_proposal_decision_proposal ON lab_proposal_decisions(proposal_id, created_at, id);
CREATE TRIGGER IF NOT EXISTS lab_proposal_immutable
BEFORE UPDATE ON lab_planning_proposals
WHEN NEW.id != OLD.id OR NEW.lab_id != OLD.lab_id
    OR NEW.interest_revision != OLD.interest_revision OR NEW.spec_json != OLD.spec_json
    OR NEW.created_at != OLD.created_at OR OLD.status != 'proposed' OR NEW.status = 'proposed'
BEGIN
    SELECT RAISE(ABORT, 'proposal content and terminal states are immutable');
END;
CREATE TRIGGER IF NOT EXISTS lab_proposal_no_delete
BEFORE DELETE ON lab_planning_proposals BEGIN
    SELECT RAISE(ABORT, 'proposals are durable');
END;
CREATE TRIGGER IF NOT EXISTS lab_proposal_decision_no_update
BEFORE UPDATE ON lab_proposal_decisions BEGIN
    SELECT RAISE(ABORT, 'proposal decisions are immutable');
END;
CREATE TRIGGER IF NOT EXISTS lab_proposal_decision_no_delete
BEFORE DELETE ON lab_proposal_decisions BEGIN
    SELECT RAISE(ABORT, 'proposal decisions are immutable');
END;
