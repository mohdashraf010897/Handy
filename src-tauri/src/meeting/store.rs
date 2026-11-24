use crate::meeting::types::{
    AudioCaptureMode, MeetingDetail, MeetingRollup, MeetingSegment, MeetingSummaryRecord,
    MeetingTemplate, OutputRouteKind, SpeakerChannel,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meetings (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  duration_ms INTEGER NOT NULL DEFAULT 0,
  template TEXT NOT NULL DEFAULT 'general',
  capture_mode TEXT NOT NULL DEFAULT 'mic_only_fallback',
  output_route TEXT NOT NULL DEFAULT 'unknown',
  app_name TEXT,
  rough_notes TEXT NOT NULL DEFAULT '',
  expanded_notes_md TEXT NOT NULL DEFAULT '',
  summary_source TEXT NOT NULL DEFAULT 'local_extractive'
);

CREATE TABLE IF NOT EXISTS meeting_segments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  speaker TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  text TEXT NOT NULL,
  language TEXT,
  speaker_id TEXT,
  speaker_label TEXT
);

CREATE INDEX IF NOT EXISTS idx_meeting_segments_meeting_start
  ON meeting_segments(meeting_id, start_ms);

CREATE TABLE IF NOT EXISTS meeting_rollups (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  window_index INTEGER NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  summary_bullets TEXT NOT NULL,
  UNIQUE(meeting_id, window_index)
);
"#;

#[derive(Debug, Clone)]
pub struct MeetingStore {
    db_path: PathBuf,
}

impl MeetingStore {
    pub fn new(app: &AppHandle) -> Result<Self, String> {
        let app_data_dir = crate::portable::app_data_dir(app)
            .map_err(|e| format!("Failed to resolve app data directory: {e}"))?;
        Self::open_in_path(app_data_dir.join("meetings.db"))
    }

    pub fn open_in_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let db_path = path.as_ref().to_path_buf();
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create meetings directory {:?}: {e}", parent))?;
        }

        let store = Self { db_path };
        store.initialize()?;
        Ok(store)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn initialize(&self) -> Result<(), String> {
        let conn = self.open_connection()?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("Failed to initialize meeting schema: {e}"))?;
        ensure_segment_speaker_columns(&conn)?;
        self.tighten_permissions()?;
        Ok(())
    }

    fn ensure_db_file_exists(&self) -> Result<(), String> {
        if self.db_path.exists() {
            #[cfg(unix)]
            {
                fs::set_permissions(&self.db_path, fs::Permissions::from_mode(0o600)).map_err(
                    |e| {
                        format!(
                            "Failed to harden existing meetings db {:?}: {e}",
                            self.db_path
                        )
                    },
                )?;
            }
            return Ok(());
        }

        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create meetings parent {:?}: {e}", parent))?;
        }

        let mut options = fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let file = options
            .open(&self.db_path)
            .map_err(|e| format!("Failed to create meetings db {:?}: {e}", self.db_path))?;

        #[cfg(unix)]
        {
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("Failed to harden meetings db permissions: {e}"))?;
        }

        Ok(())
    }

    fn open_connection(&self) -> Result<Connection, String> {
        self.ensure_db_file_exists()?;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("Failed to open meetings db {:?}: {e}", self.db_path))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA secure_delete = ON;",
        )
        .map_err(|e| format!("Failed to configure meetings db pragmas: {e}"))?;

        self.tighten_permissions()?;
        Ok(conn)
    }

    #[cfg(unix)]
    fn sidecar_path(&self, suffix: &str) -> PathBuf {
        let mut os = self.db_path.as_os_str().to_os_string();
        os.push(suffix);
        PathBuf::from(os)
    }

    fn tighten_permissions(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            fn set_0600_if_exists(path: &Path) -> Result<(), String> {
                if path.exists() {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                        .map_err(|e| format!("Failed to set 0600 on {:?}: {e}", path))?;
                }
                Ok(())
            }

            set_0600_if_exists(&self.db_path)?;
            set_0600_if_exists(&self.sidecar_path("-wal"))?;
            set_0600_if_exists(&self.sidecar_path("-shm"))?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_meeting(
        &self,
        id: &str,
        title: &str,
        started_at_ms: i64,
        template: MeetingTemplate,
        capture_mode: AudioCaptureMode,
        output_route: OutputRouteKind,
        app_name: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.open_connection()?;
        conn.execute(
            "INSERT INTO meetings (
                id, title, started_at_ms, template, capture_mode, output_route, app_name
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                title.trim(),
                started_at_ms,
                template.as_str(),
                capture_mode.as_str(),
                output_route.as_str(),
                app_name
            ],
        )
        .map_err(|e| format!("Failed to create meeting '{id}': {e}"))?;
        self.tighten_permissions()?;
        Ok(())
    }

    pub fn append_segment(
        &self,
        meeting_id: &str,
        speaker: SpeakerChannel,
        start_ms: u64,
        end_ms: u64,
        text: &str,
        language: Option<&str>,
    ) -> Result<MeetingSegment, String> {
        self.append_segment_attributed(
            meeting_id, speaker, start_ms, end_ms, text, language, None, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_segment_attributed(
        &self,
        meeting_id: &str,
        speaker: SpeakerChannel,
        start_ms: u64,
        end_ms: u64,
        text: &str,
        language: Option<&str>,
        speaker_id: Option<&str>,
        speaker_label: Option<&str>,
    ) -> Result<MeetingSegment, String> {
        let conn = self.open_connection()?;
        let start_ms_i64 = u64_to_i64(start_ms)?;
        let end_ms_i64 = u64_to_i64(end_ms)?;
        let normalized_speaker_id = speaker_id.map(str::trim).filter(|s| !s.is_empty());
        let normalized_speaker_label = speaker_label.map(str::trim).filter(|s| !s.is_empty());
        conn.execute(
            "INSERT INTO meeting_segments (
                meeting_id, speaker, start_ms, end_ms, text, language, speaker_id, speaker_label
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                meeting_id,
                speaker.as_str(),
                start_ms_i64,
                end_ms_i64,
                text.trim(),
                language,
                normalized_speaker_id,
                normalized_speaker_label,
            ],
        )
        .map_err(|e| format!("Failed to append segment for '{meeting_id}': {e}"))?;
        let id = conn.last_insert_rowid();
        self.tighten_permissions()?;

        Ok(MeetingSegment {
            id,
            meeting_id: meeting_id.to_string(),
            speaker,
            start_ms,
            end_ms,
            text: text.trim().to_string(),
            language: language.map(str::to_string),
            speaker_id: normalized_speaker_id.map(str::to_string),
            speaker_label: normalized_speaker_label.map(str::to_string),
        })
    }

    pub fn delete_segment(&self, meeting_id: &str, segment_id: i64) -> Result<bool, String> {
        let conn = self.open_connection()?;
        let removed = conn
            .execute(
                "DELETE FROM meeting_segments WHERE meeting_id = ?1 AND id = ?2",
                params![meeting_id, segment_id],
            )
            .map_err(|e| format!("Failed to delete bleed segment {segment_id}: {e}"))?;
        Ok(removed > 0)
    }

    pub fn backfill_speaker_label(
        &self,
        meeting_id: &str,
        speaker_id: &str,
        speaker_label: &str,
    ) -> Result<usize, String> {
        let trimmed_id = speaker_id.trim();
        let trimmed_label = speaker_label.trim();
        if trimmed_id.is_empty() || trimmed_label.is_empty() {
            return Ok(0);
        }

        let conn = self.open_connection()?;
        let updated = conn
            .execute(
                "UPDATE meeting_segments
                 SET speaker_label = ?1
                 WHERE meeting_id = ?2 AND speaker_id = ?3",
                params![trimmed_label, meeting_id, trimmed_id],
            )
            .map_err(|e| {
                format!("Failed to backfill speaker label for '{meeting_id}' ({trimmed_id}): {e}")
            })?;
        self.tighten_permissions()?;
        Ok(updated)
    }

    pub fn upsert_rollup(
        &self,
        meeting_id: &str,
        window_index: u32,
        start_ms: u64,
        end_ms: u64,
        summary_bullets: &str,
    ) -> Result<MeetingRollup, String> {
        let conn = self.open_connection()?;
        let window_index_i64 = i64::from(window_index);
        let start_ms_i64 = u64_to_i64(start_ms)?;
        let end_ms_i64 = u64_to_i64(end_ms)?;

        conn.execute(
            "INSERT INTO meeting_rollups (
                meeting_id, window_index, start_ms, end_ms, summary_bullets
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(meeting_id, window_index)
             DO UPDATE SET
                start_ms = excluded.start_ms,
                end_ms = excluded.end_ms,
                summary_bullets = excluded.summary_bullets",
            params![
                meeting_id,
                window_index_i64,
                start_ms_i64,
                end_ms_i64,
                summary_bullets.trim()
            ],
        )
        .map_err(|e| format!("Failed to upsert rollup for '{meeting_id}': {e}"))?;

        let id: i64 = conn
            .query_row(
                "SELECT id FROM meeting_rollups WHERE meeting_id = ?1 AND window_index = ?2",
                params![meeting_id, window_index_i64],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to fetch rollup id for '{meeting_id}': {e}"))?;

        self.tighten_permissions()?;

        Ok(MeetingRollup {
            id,
            meeting_id: meeting_id.to_string(),
            window_index,
            start_ms,
            end_ms,
            summary_bullets: summary_bullets.trim().to_string(),
        })
    }

    pub fn update_rough_notes(&self, meeting_id: &str, rough_notes: &str) -> Result<(), String> {
        let conn = self.open_connection()?;
        let changed = conn
            .execute(
                "UPDATE meetings SET rough_notes = ?2 WHERE id = ?1",
                params![meeting_id, rough_notes],
            )
            .map_err(|e| format!("Failed to update rough notes for '{meeting_id}': {e}"))?;

        if changed == 0 {
            return Err(format!("Meeting '{meeting_id}' was not found"));
        }

        self.tighten_permissions()?;
        Ok(())
    }

    pub fn finalize_meeting(
        &self,
        meeting_id: &str,
        ended_at_ms: i64,
        duration_ms: u64,
    ) -> Result<(), String> {
        let conn = self.open_connection()?;
        let duration_ms_i64 = u64_to_i64(duration_ms)?;
        let changed = conn
            .execute(
                "UPDATE meetings
                 SET ended_at_ms = ?2, duration_ms = ?3
                 WHERE id = ?1",
                params![meeting_id, ended_at_ms, duration_ms_i64],
            )
            .map_err(|e| format!("Failed to finalize meeting '{meeting_id}': {e}"))?;

        if changed == 0 {
            return Err(format!("Meeting '{meeting_id}' was not found"));
        }

        self.tighten_permissions()?;
        Ok(())
    }

    pub fn update_meeting_meta(
        &self,
        meeting_id: &str,
        title: &str,
        template: MeetingTemplate,
        rough_notes: &str,
    ) -> Result<(), String> {
        let conn = self.open_connection()?;
        let changed = conn
            .execute(
                "UPDATE meetings
                 SET title = ?2, template = ?3, rough_notes = ?4
                 WHERE id = ?1",
                params![meeting_id, title.trim(), template.as_str(), rough_notes],
            )
            .map_err(|e| format!("Failed to update metadata for '{meeting_id}': {e}"))?;

        if changed == 0 {
            return Err(format!("Meeting '{meeting_id}' was not found"));
        }

        self.tighten_permissions()?;
        Ok(())
    }

    /// Atomically replaces `title` only when the row's current title still matches
    /// `expected_placeholder_title`, never overwriting concurrent user title or `rough_notes` edits.
    pub fn update_placeholder_title_if_unchanged(
        &self,
        meeting_id: &str,
        expected_placeholder_title: &str,
        new_title: &str,
    ) -> Result<bool, String> {
        let trimmed_new = new_title.trim();
        if trimmed_new.is_empty() || trimmed_new == expected_placeholder_title {
            return Ok(false);
        }

        let conn = self.open_connection()?;
        let changed = conn
            .execute(
                "UPDATE meetings
                 SET title = ?3
                 WHERE id = ?1 AND title = ?2",
                params![meeting_id, expected_placeholder_title, trimmed_new],
            )
            .map_err(|e| format!("Failed to auto-title meeting '{meeting_id}': {e}"))?;

        if changed > 0 {
            self.tighten_permissions()?;
        }
        Ok(changed > 0)
    }

    pub fn update_expanded_notes(
        &self,
        meeting_id: &str,
        expanded_notes_md: &str,
        summary_source: &str,
    ) -> Result<(), String> {
        let conn = self.open_connection()?;
        let changed = conn
            .execute(
                "UPDATE meetings
                 SET expanded_notes_md = ?2, summary_source = ?3
                 WHERE id = ?1",
                params![meeting_id, expanded_notes_md, summary_source],
            )
            .map_err(|e| format!("Failed to update expanded notes for '{meeting_id}': {e}"))?;

        if changed == 0 {
            return Err(format!("Meeting '{meeting_id}' was not found"));
        }

        self.tighten_permissions()?;
        Ok(())
    }

    pub fn get_meeting_detail(&self, meeting_id: &str) -> Result<MeetingDetail, String> {
        let conn = self.open_connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT
                    m.id,
                    m.title,
                    m.started_at_ms,
                    m.ended_at_ms,
                    m.duration_ms,
                    m.template,
                    m.capture_mode,
                    m.output_route,
                    m.app_name,
                    m.rough_notes,
                    m.expanded_notes_md,
                    m.summary_source,
                    COALESCE((SELECT COUNT(1) FROM meeting_segments s WHERE s.meeting_id = m.id), 0)
                 FROM meetings m
                 WHERE m.id = ?1",
            )
            .map_err(|e| format!("Failed to prepare meeting detail query: {e}"))?;

        let row = stmt
            .query_row(params![meeting_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                ))
            })
            .optional()
            .map_err(|e| format!("Failed to query meeting '{meeting_id}': {e}"))?
            .ok_or_else(|| format!("Meeting '{meeting_id}' was not found"))?;

        let summary = MeetingSummaryRecord {
            id: row.0,
            title: row.1,
            started_at_ms: row.2,
            ended_at_ms: row.3,
            duration_ms: non_negative_u64(row.4),
            template: MeetingTemplate::from_db_str(&row.5),
            capture_mode: parse_capture_mode(&row.6),
            output_route: parse_output_route(&row.7),
            app_name: row.8,
            segment_count: non_negative_u32(row.12),
            preview_snippet: preview_snippet(&row.9, &row.10),
        };

        let segments = self.query_segments(
            &conn,
            "SELECT id, meeting_id, speaker, start_ms, end_ms, text, language, speaker_id, speaker_label
             FROM meeting_segments
             WHERE meeting_id = ?1
             ORDER BY start_ms ASC, id ASC",
            params![meeting_id],
        )?;

        let rollups = self.query_rollups(
            &conn,
            "SELECT id, meeting_id, window_index, start_ms, end_ms, summary_bullets
             FROM meeting_rollups
             WHERE meeting_id = ?1
             ORDER BY window_index ASC, id ASC",
            params![meeting_id],
        )?;

        Ok(MeetingDetail {
            summary,
            rough_notes: row.9,
            expanded_notes_md: row.10,
            summary_source: row.11,
            segments,
            rollups,
        })
    }

    pub fn get_segments_in_range(
        &self,
        meeting_id: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Vec<MeetingSegment>, String> {
        let conn = self.open_connection()?;
        self.query_segments(
            &conn,
            "SELECT id, meeting_id, speaker, start_ms, end_ms, text, language, speaker_id, speaker_label
             FROM meeting_segments
             WHERE meeting_id = ?1
               AND start_ms >= ?2
               AND start_ms < ?3
             ORDER BY start_ms ASC, id ASC",
            params![meeting_id, u64_to_i64(start_ms)?, u64_to_i64(end_ms)?],
        )
    }

    pub fn get_tail_segments(
        &self,
        meeting_id: &str,
        limit: usize,
    ) -> Result<Vec<MeetingSegment>, String> {
        let conn = self.open_connection()?;
        let limit_i64 =
            i64::try_from(limit).map_err(|_| "Tail segment limit is too large".to_string())?;
        let mut segments = self.query_segments(
            &conn,
            "SELECT id, meeting_id, speaker, start_ms, end_ms, text, language, speaker_id, speaker_label
             FROM meeting_segments
             WHERE meeting_id = ?1
             ORDER BY start_ms DESC, id DESC
             LIMIT ?2",
            params![meeting_id, limit_i64],
        )?;
        segments.reverse();
        Ok(segments)
    }

    pub fn list_meetings(&self, query: Option<&str>) -> Result<Vec<MeetingSummaryRecord>, String> {
        let conn = self.open_connection()?;
        let normalized_query = query.map(str::trim).filter(|q| !q.is_empty()).map(|q| {
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        });

        let mut stmt = conn
            .prepare(
                "SELECT
                    m.id,
                    m.title,
                    m.started_at_ms,
                    m.ended_at_ms,
                    m.duration_ms,
                    m.template,
                    m.capture_mode,
                    m.output_route,
                    m.app_name,
                    COALESCE((SELECT COUNT(1) FROM meeting_segments s WHERE s.meeting_id = m.id), 0),
                    COALESCE(NULLIF(TRIM(m.rough_notes), ''),
                             NULLIF(TRIM(m.expanded_notes_md), ''),
                             (SELECT s.text FROM meeting_segments s
                              WHERE s.meeting_id = m.id
                              ORDER BY s.start_ms ASC, s.id ASC
                              LIMIT 1),
                             '')
                 FROM meetings m
                 WHERE (
                    ?1 IS NULL
                    OR lower(m.title) LIKE '%' || lower(?1) || '%' ESCAPE '\\'
                    OR lower(m.rough_notes) LIKE '%' || lower(?1) || '%' ESCAPE '\\'
                    OR lower(m.expanded_notes_md) LIKE '%' || lower(?1) || '%' ESCAPE '\\'
                    OR EXISTS (
                        SELECT 1
                        FROM meeting_segments s
                        WHERE s.meeting_id = m.id
                          AND (
                            lower(s.text) LIKE '%' || lower(?1) || '%' ESCAPE '\\'
                            OR lower(COALESCE(s.speaker_label, '')) LIKE '%' || lower(?1) || '%' ESCAPE '\\'
                          )
                    )
                 )
                 ORDER BY m.started_at_ms DESC",
            )
            .map_err(|e| format!("Failed to prepare list_meetings query: {e}"))?;

        let mut rows = stmt
            .query(params![normalized_query.as_deref()])
            .map_err(|e| format!("Failed to execute list_meetings query: {e}"))?;

        let mut results = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read list_meetings row: {e}"))?
        {
            let preview: String = row.get(10).unwrap_or_default();
            results.push(MeetingSummaryRecord {
                id: row.get(0).unwrap_or_default(),
                title: row.get(1).unwrap_or_default(),
                started_at_ms: row.get(2).unwrap_or_default(),
                ended_at_ms: row.get(3).unwrap_or(None),
                duration_ms: non_negative_u64(row.get::<_, i64>(4).unwrap_or_default()),
                template: MeetingTemplate::from_db_str(
                    &row.get::<_, String>(5).unwrap_or_default(),
                ),
                capture_mode: parse_capture_mode(&row.get::<_, String>(6).unwrap_or_default()),
                output_route: parse_output_route(&row.get::<_, String>(7).unwrap_or_default()),
                app_name: row.get(8).unwrap_or(None),
                segment_count: non_negative_u32(row.get::<_, i64>(9).unwrap_or_default()),
                preview_snippet: preview.trim().chars().take(220).collect(),
            });
        }

        Ok(results)
    }

    pub fn delete_meeting(&self, meeting_id: &str) -> Result<bool, String> {
        let conn = self.open_connection()?;
        let affected = conn
            .execute("DELETE FROM meetings WHERE id = ?1", params![meeting_id])
            .map_err(|e| format!("Failed to delete meeting '{meeting_id}': {e}"))?;
        self.tighten_permissions()?;
        Ok(affected > 0)
    }

    /// Lists meeting IDs whose session exited or crashed before `finalize_meeting`
    /// set `ended_at_ms`.
    pub fn list_orphaned_meeting_ids(&self) -> Result<Vec<String>, String> {
        let conn = self.open_connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id FROM meetings
                 WHERE ended_at_ms IS NULL
                 ORDER BY started_at_ms ASC",
            )
            .map_err(|e| format!("Failed to prepare list_orphaned_meeting_ids query: {e}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("Failed to query orphaned meeting ids: {e}"))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|e| format!("Failed to read orphaned meeting id: {e}"))?);
        }
        Ok(ids)
    }

    pub fn format_meeting_markdown(detail: &MeetingDetail) -> String {
        let mut out = String::new();
        let summary = &detail.summary;

        out.push_str("---\n");
        out.push_str(&format!("id: {}\n", sanitize_yaml_scalar(&summary.id)));
        out.push_str(&format!(
            "title: {}\n",
            sanitize_yaml_scalar(&summary.title)
        ));
        out.push_str(&format!("template: {}\n", summary.template.as_str()));
        out.push_str(&format!("started_at_ms: {}\n", summary.started_at_ms));
        match summary.ended_at_ms {
            Some(value) => out.push_str(&format!("ended_at_ms: {}\n", value)),
            None => out.push_str("ended_at_ms: null\n"),
        }
        out.push_str(&format!("duration_ms: {}\n", summary.duration_ms));
        out.push_str(&format!(
            "capture_mode: {}\n",
            summary.capture_mode.as_str()
        ));
        out.push_str(&format!(
            "output_route: {}\n",
            summary.output_route.as_str()
        ));
        match &summary.app_name {
            Some(name) if !name.trim().is_empty() => {
                out.push_str(&format!("app_name: {}\n", sanitize_yaml_scalar(name)));
            }
            _ => out.push_str("app_name: null\n"),
        }
        out.push_str(&format!(
            "summary_source: {}\n",
            sanitize_yaml_scalar(&detail.summary_source)
        ));
        out.push_str("---\n\n");

        out.push_str(&format!("# {}\n\n", summary.title.trim()));

        out.push_str("## Expanded Notes\n\n");
        if detail.expanded_notes_md.trim().is_empty() {
            out.push_str("_No expanded notes generated._\n\n");
        } else {
            out.push_str(detail.expanded_notes_md.trim());
            out.push_str("\n\n");
        }

        if !detail.rough_notes.trim().is_empty() {
            out.push_str("## Your Rough Bullets\n\n");
            for bullet in rough_notes_bullets(&detail.rough_notes) {
                out.push_str("- ");
                out.push_str(&bullet);
                out.push('\n');
            }
            out.push('\n');
        }

        out.push_str("## Transcript\n\n");
        if detail.segments.is_empty() {
            out.push_str("_No transcript segments captured._\n");
        } else {
            for segment in &detail.segments {
                let ts = format_mm_ss(segment.start_ms);
                out.push_str(&format!(
                    "- [{}] **{}**: {}\n",
                    ts,
                    segment.effective_speaker_label(),
                    segment.text.trim()
                ));
            }
        }

        out
    }

    fn query_segments<P>(
        &self,
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> Result<Vec<MeetingSegment>, String>
    where
        P: rusqlite::Params,
    {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare segment query: {e}"))?;
        let mut rows = stmt
            .query(params)
            .map_err(|e| format!("Failed to run segment query: {e}"))?;

        let mut segments = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read segment row: {e}"))?
        {
            let speaker_raw: String = row.get(2).unwrap_or_else(|_| "me".to_string());
            let start_ms = non_negative_u64(row.get::<_, i64>(3).unwrap_or_default());
            let end_ms = non_negative_u64(row.get::<_, i64>(4).unwrap_or_default());

            segments.push(MeetingSegment {
                id: row.get(0).unwrap_or_default(),
                meeting_id: row.get(1).unwrap_or_default(),
                speaker: parse_speaker(&speaker_raw),
                start_ms,
                end_ms,
                text: row.get(5).unwrap_or_default(),
                language: row.get(6).unwrap_or(None),
                speaker_id: row.get(7).unwrap_or(None),
                speaker_label: row.get(8).unwrap_or(None),
            });
        }

        Ok(segments)
    }

    fn query_rollups<P>(
        &self,
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> Result<Vec<MeetingRollup>, String>
    where
        P: rusqlite::Params,
    {
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| format!("Failed to prepare rollup query: {e}"))?;
        let mut rows = stmt
            .query(params)
            .map_err(|e| format!("Failed to run rollup query: {e}"))?;

        let mut rollups = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("Failed to read rollup row: {e}"))?
        {
            rollups.push(MeetingRollup {
                id: row.get(0).unwrap_or_default(),
                meeting_id: row.get(1).unwrap_or_default(),
                window_index: non_negative_u32(row.get::<_, i64>(2).unwrap_or_default()),
                start_ms: non_negative_u64(row.get::<_, i64>(3).unwrap_or_default()),
                end_ms: non_negative_u64(row.get::<_, i64>(4).unwrap_or_default()),
                summary_bullets: row.get(5).unwrap_or_default(),
            });
        }

        Ok(rollups)
    }
}

fn ensure_segment_speaker_columns(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(meeting_segments)")
        .map_err(|e| format!("Failed to inspect meeting_segments columns: {e}"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("Failed to query meeting_segments columns: {e}"))?;

    let mut has_speaker_id = false;
    let mut has_speaker_label = false;
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to read meeting_segments column info: {e}"))?
    {
        let name: String = row.get(1).unwrap_or_default();
        match name.as_str() {
            "speaker_id" => has_speaker_id = true,
            "speaker_label" => has_speaker_label = true,
            _ => {}
        }
    }
    drop(rows);
    drop(stmt);

    if !has_speaker_id {
        conn.execute_batch("ALTER TABLE meeting_segments ADD COLUMN speaker_id TEXT;")
            .map_err(|e| format!("Failed to migrate meeting_segments.speaker_id: {e}"))?;
    }
    if !has_speaker_label {
        conn.execute_batch("ALTER TABLE meeting_segments ADD COLUMN speaker_label TEXT;")
            .map_err(|e| format!("Failed to migrate meeting_segments.speaker_label: {e}"))?;
    }
    Ok(())
}

fn u64_to_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("Value {value} exceeds i64 range"))
}

fn non_negative_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn non_negative_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

fn parse_capture_mode(value: &str) -> AudioCaptureMode {
    match value {
        "dual_stream" => AudioCaptureMode::DualStream,
        _ => AudioCaptureMode::MicOnlyFallback,
    }
}

fn parse_output_route(value: &str) -> OutputRouteKind {
    match value {
        "headphones" => OutputRouteKind::Headphones,
        "speakers" => OutputRouteKind::Speakers,
        _ => OutputRouteKind::Unknown,
    }
}

fn parse_speaker(value: &str) -> SpeakerChannel {
    match value {
        "others" => SpeakerChannel::Others,
        _ => SpeakerChannel::Me,
    }
}

fn preview_snippet(rough_notes: &str, expanded_notes_md: &str) -> String {
    let source = if !rough_notes.trim().is_empty() {
        rough_notes
    } else {
        expanded_notes_md
    };
    source.trim().chars().take(220).collect()
}

fn sanitize_yaml_scalar(value: &str) -> String {
    let escaped = value.replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

fn rough_notes_bullets(raw: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed
            .trim_start_matches(['-', '*', '•', '–', '—'])
            .trim_start()
            .to_string();
        if !normalized.is_empty() {
            bullets.push(normalized);
        }
    }

    if bullets.is_empty() && !raw.trim().is_empty() {
        bullets.push(raw.trim().to_string());
    }

    bullets
}

fn format_mm_ss(ms: u64) -> String {
    let total_sec = ms / 1_000;
    let minutes = total_sec / 60;
    let seconds = total_sec % 60;
    format!("{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_store() -> (tempfile::TempDir, MeetingStore) {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("meetings.db");
        let store = MeetingStore::open_in_path(&db_path).expect("create store");
        (dir, store)
    }

    fn seed_meeting(store: &MeetingStore, meeting_id: &str) {
        store
            .create_meeting(
                meeting_id,
                "Weekly Sync",
                1_700_000_000_000,
                MeetingTemplate::General,
                AudioCaptureMode::DualStream,
                OutputRouteKind::Headphones,
                Some("Zoom"),
            )
            .expect("create meeting");
    }

    #[test]
    fn meeting_store_should_crud_and_cascade_delete() {
        let (_dir, store) = create_store();
        seed_meeting(&store, "m1");

        let first = store
            .append_segment(
                "m1",
                SpeakerChannel::Me,
                0,
                12_000,
                "Let's ship this today.",
                Some("en"),
            )
            .expect("append first segment");
        assert!(first.id > 0);

        store
            .append_segment(
                "m1",
                SpeakerChannel::Others,
                12_000,
                25_000,
                "Need to verify risks.",
                Some("en"),
            )
            .expect("append second segment");

        store
            .upsert_rollup(
                "m1",
                0,
                0,
                300_000,
                "- [00:00] **Me**: Let's ship this today.",
            )
            .expect("upsert rollup");

        store
            .update_rough_notes("m1", "- ship today")
            .expect("update rough notes");
        store
            .update_expanded_notes("m1", "## TL;DR\n\nShipped.", "local_extractive")
            .expect("update expanded notes");
        store
            .finalize_meeting("m1", 1_700_000_090_000, 90_000)
            .expect("finalize");

        let detail = store.get_meeting_detail("m1").expect("detail");
        assert_eq!(detail.summary.segment_count, 2);
        assert_eq!(detail.rollups.len(), 1);
        assert_eq!(detail.summary.duration_ms, 90_000);

        assert!(store.delete_meeting("m1").expect("delete meeting"));

        let conn = Connection::open(store.db_path()).expect("open db");
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .expect("enable foreign keys");
        let seg_count: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM meeting_segments WHERE meeting_id = 'm1'",
                [],
                |row| row.get(0),
            )
            .expect("segment count");
        let rollup_count: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM meeting_rollups WHERE meeting_id = 'm1'",
                [],
                |row| row.get(0),
            )
            .expect("rollup count");

        assert_eq!(seg_count, 0);
        assert_eq!(rollup_count, 0);
        assert!(store.get_meeting_detail("m1").is_err());
    }

    #[test]
    fn list_meetings_should_search_rough_notes_and_transcript_case_insensitively() {
        let (_dir, store) = create_store();
        seed_meeting(&store, "m1");
        seed_meeting(&store, "m2");

        store
            .update_rough_notes("m1", "Need action from finance")
            .expect("update rough notes");

        store
            .append_segment(
                "m2",
                SpeakerChannel::Others,
                0,
                5_000,
                "Client asked about security posture",
                Some("en"),
            )
            .expect("append segment");

        let rough_hits = store.list_meetings(Some("FINANCE")).expect("rough search");
        assert_eq!(rough_hits.len(), 1);
        assert_eq!(rough_hits[0].id, "m1");

        let transcript_hits = store
            .list_meetings(Some("security"))
            .expect("transcript search");
        assert_eq!(transcript_hits.len(), 1);
        assert_eq!(transcript_hits[0].id, "m2");
    }

    #[test]
    fn format_meeting_markdown_should_include_metadata_and_timestamped_transcript() {
        let (_dir, store) = create_store();
        seed_meeting(&store, "m1");
        store
            .append_segment(
                "m1",
                SpeakerChannel::Me,
                9_000,
                14_000,
                "Let's ship Wave 3.",
                Some("en"),
            )
            .expect("append");
        store
            .update_rough_notes("m1", "- finalize checklist")
            .expect("rough");
        store
            .update_expanded_notes("m1", "## TL;DR\n\nWave 3 is on track.", "local_extractive")
            .expect("expanded");

        let detail = store.get_meeting_detail("m1").expect("detail");
        let markdown = MeetingStore::format_meeting_markdown(&detail);

        assert!(markdown.contains("summary_source:"));
        assert!(markdown.contains("## Expanded Notes"));
        assert!(markdown.contains("## Your Rough Bullets"));
        assert!(markdown.contains("[00:09] **Me**: Let's ship Wave 3."));
    }

    #[test]
    #[cfg(unix)]
    fn meeting_store_should_harden_permissions_to_0600_on_unix() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, store) = create_store();
        seed_meeting(&store, "m1");
        store
            .append_segment("m1", SpeakerChannel::Me, 0, 12_000, "test", Some("en"))
            .expect("append");

        let db_mode = fs::metadata(store.db_path()).expect("db metadata").mode() & 0o777;
        assert_eq!(db_mode, 0o600);

        for suffix in ["-wal", "-shm"] {
            let path = store.sidecar_path(suffix);
            if path.exists() {
                let mode = fs::metadata(&path).expect("sidecar metadata").mode() & 0o777;
                assert_eq!(mode, 0o600, "unexpected mode for {:?}", path);
            }
        }
    }

    #[test]
    fn legacy_meeting_segments_migration_is_idempotent_and_preserves_rows_and_0600_mode() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("legacy_meetings.db");

        {
            let conn = Connection::open(&db_path).expect("open legacy db");
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE meetings (
                   id TEXT PRIMARY KEY,
                   title TEXT NOT NULL,
                   started_at_ms INTEGER NOT NULL,
                   ended_at_ms INTEGER,
                   duration_ms INTEGER NOT NULL DEFAULT 0,
                   template TEXT NOT NULL DEFAULT 'general',
                   capture_mode TEXT NOT NULL DEFAULT 'mic_only_fallback',
                   output_route TEXT NOT NULL DEFAULT 'unknown',
                   app_name TEXT,
                   rough_notes TEXT NOT NULL DEFAULT '',
                   expanded_notes_md TEXT NOT NULL DEFAULT '',
                   summary_source TEXT NOT NULL DEFAULT 'local_extractive'
                 );
                 CREATE TABLE meeting_segments (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
                   speaker TEXT NOT NULL,
                   start_ms INTEGER NOT NULL,
                   end_ms INTEGER NOT NULL,
                   text TEXT NOT NULL,
                   language TEXT
                 );
                 INSERT INTO meetings (id, title, started_at_ms)
                   VALUES ('legacy-1', 'Legacy Sync', 1700000000000);
                 INSERT INTO meeting_segments (meeting_id, speaker, start_ms, end_ms, text, language)
                   VALUES ('legacy-1', 'others', 2000, 6000, 'Existing pre-migration turn', 'en');",
            )
            .expect("seed legacy schema");
        }

        // Open twice to prove idempotency.
        let store_first = MeetingStore::open_in_path(&db_path).expect("migrate first time");
        let store = MeetingStore::open_in_path(&db_path).expect("migrate second time");
        drop(store_first);

        let detail = store
            .get_meeting_detail("legacy-1")
            .expect("load migrated meeting");
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].text, "Existing pre-migration turn");
        assert_eq!(detail.segments[0].speaker_id, None);
        assert_eq!(detail.segments[0].speaker_label, None);
        assert_eq!(detail.segments[0].effective_speaker_label(), "Others");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let db_mode = fs::metadata(store.db_path()).expect("db metadata").mode() & 0o777;
            assert_eq!(db_mode, 0o600);
            for suffix in ["-wal", "-shm"] {
                let path = store.sidecar_path(suffix);
                if path.exists() {
                    let mode = fs::metadata(&path).expect("sidecar metadata").mode() & 0o777;
                    assert_eq!(mode, 0o600, "unexpected mode for {:?}", path);
                }
            }
        }
    }

    #[test]
    fn append_segment_attributed_and_backfill_speaker_label_round_trip_cleanly() {
        let (_dir, store) = create_store();
        seed_meeting(&store, "m-attr");
        seed_meeting(&store, "m-other");

        let first = store
            .append_segment_attributed(
                "m-attr",
                SpeakerChannel::Others,
                0,
                4_000,
                "Good morning team.",
                Some("en"),
                Some("speaker_1"),
                Some("Speaker 1"),
            )
            .expect("append speaker_1 turn 1");
        assert_eq!(first.speaker_id.as_deref(), Some("speaker_1"));
        assert_eq!(first.speaker_label.as_deref(), Some("Speaker 1"));
        assert_eq!(first.effective_speaker_label(), "Speaker 1");

        store
            .append_segment_attributed(
                "m-attr",
                SpeakerChannel::Others,
                4_500,
                8_000,
                "I will review the API checklist.",
                Some("en"),
                Some("speaker_2"),
                Some("Speaker 2"),
            )
            .expect("append speaker_2");

        store
            .append_segment_attributed(
                "m-attr",
                SpeakerChannel::Me,
                8_500,
                11_000,
                "Thanks Rahul, go ahead.",
                Some("en"),
                Some("me"),
                Some("Me"),
            )
            .expect("append me");

        store
            .append_segment_attributed(
                "m-other",
                SpeakerChannel::Others,
                0,
                3_000,
                "Unrelated meeting speaker_1.",
                Some("en"),
                Some("speaker_1"),
                Some("Speaker 1"),
            )
            .expect("append other meeting speaker_1");

        store
            .append_segment_attributed(
                "m-attr",
                SpeakerChannel::Others,
                11_500,
                15_000,
                "Sure, sharing my screen now.",
                Some("en"),
                Some("speaker_1"),
                Some("Speaker 1 (Rahul)"),
            )
            .expect("append speaker_1 turn 2");

        let updated = store
            .backfill_speaker_label("m-attr", "speaker_1", "Speaker 1 (Rahul)")
            .expect("backfill speaker_1");
        assert_eq!(updated, 2);

        // Atomic CAS title update succeeds when placeholder matches, and preserves rough_notes / manual edits.
        store
            .create_meeting(
                "m-cas",
                "Meeting — 2026-11-21 09:30",
                1_700_000_000_000,
                MeetingTemplate::General,
                AudioCaptureMode::DualStream,
                OutputRouteKind::Unknown,
                Some("zoom.us"),
            )
            .expect("create m-cas");
        store
            .update_rough_notes("m-cas", "fresh note saved while summarizing")
            .expect("update rough notes");
        assert!(store
            .update_placeholder_title_if_unchanged(
                "m-cas",
                "Meeting — 2026-11-21 09:30",
                "Q4 Launch Readiness Sync",
            )
            .expect("cas succeeds on matching placeholder"));
        let cas_detail = store.get_meeting_detail("m-cas").expect("get m-cas");
        assert_eq!(cas_detail.summary.title, "Q4 Launch Readiness Sync");
        assert_eq!(cas_detail.rough_notes, "fresh note saved while summarizing");
        assert!(!store
            .update_placeholder_title_if_unchanged(
                "m-cas",
                "Meeting — 2026-11-21 09:30",
                "Stale Auto Title",
            )
            .expect("cas no-ops when user already renamed"));
        assert_eq!(
            store
                .get_meeting_detail("m-cas")
                .expect("re-read m-cas")
                .summary
                .title,
            "Q4 Launch Readiness Sync"
        );

        let detail = store.get_meeting_detail("m-attr").expect("detail m-attr");
        assert_eq!(detail.segments.len(), 4);
        assert_eq!(
            detail.segments[0].speaker_label.as_deref(),
            Some("Speaker 1 (Rahul)")
        );
        assert_eq!(
            detail.segments[1].speaker_label.as_deref(),
            Some("Speaker 2")
        );
        assert_eq!(detail.segments[2].speaker_label.as_deref(), Some("Me"));
        assert_eq!(
            detail.segments[3].speaker_label.as_deref(),
            Some("Speaker 1 (Rahul)")
        );

        let other_detail = store.get_meeting_detail("m-other").expect("detail m-other");
        assert_eq!(
            other_detail.segments[0].speaker_label.as_deref(),
            Some("Speaker 1")
        );

        let markdown = MeetingStore::format_meeting_markdown(&detail);
        assert!(markdown.contains("[00:00] **Speaker 1 (Rahul)**: Good morning team."));
        assert!(markdown.contains("[00:04] **Speaker 2**: I will review the API checklist."));
        assert!(markdown.contains("[00:08] **Me**: Thanks Rahul, go ahead."));
    }
}
