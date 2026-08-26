CREATE TABLE cloud_workspaces (
    workspace_id UUID PRIMARY KEY,
    workspace_slug TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE cloud_project_snapshots (
    workspace_id UUID NOT NULL REFERENCES cloud_workspaces(workspace_id) ON DELETE CASCADE,
    project_slug TEXT NOT NULL,
    project_name TEXT NOT NULL,
    snapshot JSONB NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    snapshot_hash TEXT NOT NULL,
    pushed_by TEXT NOT NULL,
    previous_revision BIGINT,
    previous_snapshot_hash TEXT,
    forced BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, project_slug),
    CHECK (snapshot_hash ~ '^[0-9a-f]{64}$')
);

CREATE TABLE cloud_documents (
    workspace_id UUID NOT NULL,
    document_key TEXT NOT NULL,
    project_slug TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('note', 'markdown')),
    scope TEXT,
    group_slug TEXT,
    relative_path TEXT,
    label TEXT,
    content TEXT NOT NULL,
    search_vector TSVECTOR GENERATED ALWAYS AS (
        to_tsvector(
            'simple'::regconfig,
            coalesce(label, '') || ' ' || content
        )
    ) STORED,
    PRIMARY KEY (workspace_id, document_key),
    FOREIGN KEY (workspace_id, project_slug)
        REFERENCES cloud_project_snapshots(workspace_id, project_slug)
        ON DELETE CASCADE
);

CREATE INDEX cloud_project_snapshots_workspace_updated
    ON cloud_project_snapshots(workspace_id, updated_at DESC, project_slug);
CREATE INDEX cloud_documents_project
    ON cloud_documents(workspace_id, project_slug, document_key);
CREATE INDEX cloud_documents_search
    ON cloud_documents USING GIN(search_vector);

ALTER TABLE cloud_workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_workspaces FORCE ROW LEVEL SECURITY;
ALTER TABLE cloud_project_snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_project_snapshots FORCE ROW LEVEL SECURITY;
ALTER TABLE cloud_documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE cloud_documents FORCE ROW LEVEL SECURITY;

CREATE POLICY cloud_workspaces_tenant ON cloud_workspaces
    USING (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    )
    WITH CHECK (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    );

CREATE POLICY cloud_project_snapshots_tenant ON cloud_project_snapshots
    USING (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    )
    WITH CHECK (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    );

CREATE POLICY cloud_documents_tenant ON cloud_documents
    USING (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    )
    WITH CHECK (
        workspace_id = nullif(current_setting('app.workspace_id', true), '')::uuid
    );
