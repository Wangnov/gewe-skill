CREATE TABLE IF NOT EXISTS raw_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  received_at TEXT NOT NULL,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  type_name TEXT,
  msg_id TEXT,
  new_msg_id TEXT,
  msg_type INTEGER,
  dedupe_key TEXT NOT NULL UNIQUE,
  body_sha256 TEXT NOT NULL,
  event_json TEXT NOT NULL,
  source_ip TEXT,
  user_agent TEXT,
  raw_object_key TEXT,
  schema_version TEXT,
  duplicate_count INTEGER NOT NULL DEFAULT 0,
  last_seen_at TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_raw_events_received_at ON raw_events(received_at);
CREATE INDEX IF NOT EXISTS idx_raw_events_appid_new_msg_id ON raw_events(appid, new_msg_id);
CREATE INDEX IF NOT EXISTS idx_raw_events_schema_type ON raw_events(schema_version, type_name);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_key TEXT NOT NULL UNIQUE,
  raw_event_id INTEGER NOT NULL,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  type_name TEXT,
  msg_id TEXT,
  new_msg_id TEXT,
  msg_type INTEGER,
  appmsg_type INTEGER,
  from_user TEXT,
  to_user TEXT,
  conversation_id TEXT,
  sender_wxid TEXT,
  is_group INTEGER NOT NULL DEFAULT 0,
  is_outgoing INTEGER NOT NULL DEFAULT 0,
  wechat_created_at INTEGER,
  received_at TEXT NOT NULL,
  last_seen_at TEXT,
  content_text TEXT,
  content_xml TEXT,
  push_content TEXT,
  msg_source TEXT,
  schema_version TEXT,
  duplicate_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  FOREIGN KEY(raw_event_id) REFERENCES raw_events(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_messages_received_at ON messages(received_at);
CREATE INDEX IF NOT EXISTS idx_messages_conversation_received ON messages(conversation_id, received_at);
CREATE INDEX IF NOT EXISTS idx_messages_sender_received ON messages(sender_wxid, received_at);
CREATE INDEX IF NOT EXISTS idx_messages_schema_type ON messages(schema_version, type_name);

CREATE TABLE IF NOT EXISTS download_jobs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_key TEXT NOT NULL UNIQUE,
  message_id INTEGER NOT NULL,
  raw_event_id INTEGER NOT NULL,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  asset_type TEXT NOT NULL,
  variant TEXT,
  endpoint TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  claimed_at TEXT,
  completed_at TEXT,
  updated_at TEXT,
  local_path TEXT,
  r2_object_key TEXT,
  source_url TEXT,
  source_url_expires_at TEXT,
  size_bytes INTEGER,
  mime_type TEXT,
  last_error TEXT,
  FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE,
  FOREIGN KEY(raw_event_id) REFERENCES raw_events(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_download_jobs_status_created ON download_jobs(status, created_at);
CREATE INDEX IF NOT EXISTS idx_download_jobs_message ON download_jobs(message_id);

CREATE TABLE IF NOT EXISTS webhook_checks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  body_text TEXT,
  source_ip TEXT,
  user_agent TEXT
);

CREATE INDEX IF NOT EXISTS idx_webhook_checks_received_at ON webhook_checks(received_at);

CREATE TABLE IF NOT EXISTS chatroom_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  raw_event_id INTEGER NOT NULL UNIQUE,
  message_id INTEGER NOT NULL,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  chatroom_id TEXT NOT NULL,
  chatroom_name TEXT,
  chatroom_version INTEGER,
  member_count INTEGER NOT NULL,
  members_json TEXT NOT NULL,
  member_hash TEXT NOT NULL,
  received_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  FOREIGN KEY(raw_event_id) REFERENCES raw_events(id) ON DELETE CASCADE,
  FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chatroom_snapshots_chatroom_received ON chatroom_snapshots(chatroom_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_chatroom_snapshots_chatroom_version ON chatroom_snapshots(chatroom_id, chatroom_version);

CREATE TABLE IF NOT EXISTS chatroom_member_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_key TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  chatroom_id TEXT NOT NULL,
  member_wxid TEXT,
  previous_chatroom_name TEXT,
  current_chatroom_name TEXT,
  previous_member_count INTEGER,
  current_member_count INTEGER,
  previous_snapshot_id INTEGER,
  current_snapshot_id INTEGER NOT NULL,
  raw_event_id INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  received_at TEXT NOT NULL,
  details_json TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  FOREIGN KEY(previous_snapshot_id) REFERENCES chatroom_snapshots(id) ON DELETE SET NULL,
  FOREIGN KEY(current_snapshot_id) REFERENCES chatroom_snapshots(id) ON DELETE CASCADE,
  FOREIGN KEY(raw_event_id) REFERENCES raw_events(id) ON DELETE CASCADE,
  FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chatroom_member_events_chatroom_received ON chatroom_member_events(chatroom_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_chatroom_member_events_member_received ON chatroom_member_events(member_wxid, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_chatroom_member_events_type_received ON chatroom_member_events(event_type, received_at DESC);

CREATE TABLE IF NOT EXISTS chatroom_system_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  raw_event_id INTEGER NOT NULL UNIQUE,
  message_id INTEGER,
  appid TEXT NOT NULL,
  account_wxid TEXT,
  chatroom_id TEXT NOT NULL,
  event_type TEXT NOT NULL,
  actor_wxid TEXT,
  actor_name TEXT,
  target_wxid TEXT,
  target_name TEXT,
  target_wxids_json TEXT,
  target_names_json TEXT,
  previous_value TEXT,
  current_value TEXT,
  template_text TEXT,
  content_text TEXT,
  received_at TEXT NOT NULL,
  details_json TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  FOREIGN KEY(raw_event_id) REFERENCES raw_events(id) ON DELETE CASCADE,
  FOREIGN KEY(message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chatroom_system_events_chatroom_received ON chatroom_system_events(chatroom_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_chatroom_system_events_type_received ON chatroom_system_events(event_type, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_chatroom_system_events_actor ON chatroom_system_events(actor_wxid);
CREATE INDEX IF NOT EXISTS idx_chatroom_system_events_target ON chatroom_system_events(target_wxid);
