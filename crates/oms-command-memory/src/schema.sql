        create table if not exists memory_sources (
          source text primary key check (source in ('shared', 'pins', 'failures')),
          path text not null,
          size integer not null,
          mtime_ns integer not null,
          digest text not null
        );
        create table if not exists memory_entries (
          id integer primary key,
          event_id text not null unique,
          source text not null check (source in ('shared', 'pins', 'failures')),
          ordinal integer not null,
          occurred_at text not null,
          agent text not null,
          kind text not null,
          task_id text not null,
          session_hash text not null,
          git_sha text not null,
          git_dirty integer check (git_dirty is null or git_dirty in (0, 1)),
          git_state text not null,
          body text not null,
          rendered text not null,
          unique (source, ordinal)
        );
        create index if not exists memory_entries_agent
          on memory_entries (agent);
        create index if not exists memory_entries_time
          on memory_entries (occurred_at);
        create unique index if not exists memory_entries_event
          on memory_entries (event_id);
        create index if not exists memory_entries_task
          on memory_entries (task_id);
        create index if not exists memory_entries_git_sha
          on memory_entries (git_sha);
        -- Access stats live OUTSIDE memory_entries: entry rows are re-derived
        -- from Markdown (and dropped wholesale on schema migration), while
        -- what was recalled and when must survive that. Keyed by the stable
        -- content-derived event_id. An explicit `rebuild` starts a fresh file
        -- and forfeits these stats; recall quality degrades gracefully.
        create table if not exists memory_access (
          event_id text primary key,
          last_accessed text not null,
          hit_count integer not null default 0
        );
            create virtual table if not exists memory_fts using fts5(
              rendered,
              agent,
              content='memory_entries',
              content_rowid='id',
              tokenize='unicode61'
            );
            create trigger if not exists memory_entries_ai after insert
            on memory_entries begin
              insert into memory_fts(rowid, rendered, agent)
              values (new.id, new.rendered, new.agent);
            end;
            create trigger if not exists memory_entries_ad after delete
            on memory_entries begin
              insert into memory_fts(memory_fts, rowid, rendered, agent)
              values ('delete', old.id, old.rendered, old.agent);
            end;
            create trigger if not exists memory_entries_au after update
            on memory_entries begin
              insert into memory_fts(memory_fts, rowid, rendered, agent)
              values ('delete', old.id, old.rendered, old.agent);
              insert into memory_fts(rowid, rendered, agent)
              values (new.id, new.rendered, new.agent);
            end;

pragma user_version = 3;
