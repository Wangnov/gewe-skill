// @ts-nocheck

const JSON_HEADERS = { "content-type": "application/json; charset=utf-8" };
const MAX_PREVIEW_CHARS = 500;
const DOWNLOAD_ASSET_TYPES = ["image", "voice", "video", "emoji", "file"];

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);

    if (request.method === "GET" && url.pathname === "/health") {
      return json({ ok: true, service: "gewe-skill-edge", now: new Date().toISOString() });
    }

    if (request.method === "POST" && url.pathname === `/callback/${env.CALLBACK_SECRET}`) {
      return handleCallback(request, env, ctx);
    }

    if (url.pathname.startsWith("/admin/")) {
      const unauthorized = await requireAdmin(request, env);
      if (unauthorized) return unauthorized;
      return handleAdmin(request, env, url);
    }

    return new Response("Not found", { status: 404 });
  },

  async queue(batch, env) {
    for (const message of batch.messages) {
      try {
        await processDownloadJob(message.body, env);
        message.ack();
      } catch (error) {
        console.error("download job failed", safeError(error), message.body?.jobKey);
        message.retry();
      }
    }
  },

  async scheduled(event, env, ctx) {
    ctx.waitUntil(scheduledMaintenance(env, event?.cron));
  }
};

async function handleCallback(request, env, ctx) {
  const receivedAt = new Date().toISOString();
  const sourceIp = request.headers.get("cf-connecting-ip") || request.headers.get("x-forwarded-for") || null;
  const userAgent = request.headers.get("user-agent") || null;
  const bodyText = await request.text();

  let body;
  try {
    body = JSON.parse(bodyText || "{}");
  } catch (_error) {
    await recordWebhookCheck(env, bodyText.slice(0, 5000), sourceIp, userAgent);
    return json({ ok: true, stored: "webhook_check", reason: "invalid_json" });
  }

  const normalized = normalizeCallback(body, receivedAt);
  if (!normalized.appid || !normalized.dedupeKey) {
    await recordWebhookCheck(env, bodyText.slice(0, 5000), sourceIp, userAgent);
    return json({ ok: true, stored: "webhook_check", reason: "missing_message_identity" });
  }

  if (env.EXPECTED_APP_ID && normalized.appid !== env.EXPECTED_APP_ID) {
    await recordWebhookCheck(env, bodyText.slice(0, 5000), sourceIp, userAgent);
    return json({ ok: false, reason: "unexpected_appid" }, 403);
  }

  const bodySha256 = await sha256Hex(bodyText);
  const storageBodyText = redactSensitiveBody(bodyText);
  const rawObjectKey = rawEventKey(normalized.schemaVersion, normalized.dedupeKey, receivedAt);
  ctx.waitUntil(env.RAW_BUCKET.put(rawObjectKey, storageBodyText, {
    httpMetadata: { contentType: "application/json; charset=utf-8" },
    customMetadata: compactMetadata({
      appid: normalized.appid,
      wxid: normalized.accountWxid,
      schema: normalized.schemaVersion,
      new_msg_id: normalized.newMsgId,
      msg_type: String(normalized.msgType || "")
    })
  }));

  const rawEvent = await upsertRawEvent(env, normalized, storageBodyText, bodySha256, rawObjectKey, sourceIp, userAgent, receivedAt);
  const message = await upsertMessage(env, normalized, rawEvent.id, receivedAt);
  ctx.waitUntil(forwardRawEventToMemory(env, body, receivedAt));

  if (rawEvent.duplicate_count === 0) {
    await processChatroomSnapshot(env, normalized, rawEvent.id, message.id, receivedAt);
    await processChatroomSystemEvent(env, normalized, rawEvent.id, message.id, receivedAt);

    const job = buildDownloadJob(normalized, rawEvent.id, message.id, env);
    if (job) {
      await enqueueDownloadJob(env, job);
    }
  }

  return json({ ok: true, schema: normalized.schemaVersion, raw_event_id: rawEvent.id, message_id: message.id });
}

async function forwardRawEventToMemory(env, body, receivedAt) {
  if (!env.MEMORY_API_URL || !env.MEMORY_WRITE_TOKEN) return;

  try {
    const response = await fetch(`${String(env.MEMORY_API_URL).replace(/\/+$/, "")}/write/raw-events`, {
      method: "POST",
      headers: {
        "authorization": `Bearer ${env.MEMORY_WRITE_TOKEN}`,
        "content-type": "application/json; charset=utf-8"
      },
      body: JSON.stringify({
        received_at: receivedAt,
        body: redactSensitiveValue(body)
      })
    });
    if (!response.ok) {
      console.error("memory forward failed", response.status, (await response.text()).slice(0, 500));
    }
  } catch (error) {
    console.error("memory forward failed", safeError(error));
  }
}

async function handleAdmin(request, env, url) {
  if (url.pathname === "/admin/retry-downloads" && request.method === "POST") {
    return retryDownloadJobs(env, url);
  }

  if (url.pathname === "/admin/requeue-downloads" && request.method === "POST") {
    return json({ ok: true, ...(await requeueDueDownloadJobs(env)) });
  }

  if (url.pathname === "/admin/redact-raw-events" && request.method === "POST") {
    return redactStoredRawEvents(env, url);
  }

  if (url.pathname === "/admin/backfill-chatroom-events" && request.method === "POST") {
    return handleAdminBackfillChatroomEvents(request, env);
  }

  if (url.pathname === "/admin/backfill-download-jobs" && request.method === "POST") {
    return handleAdminBackfillDownloadJobs(env, url);
  }

  if (request.method !== "GET") return json({ ok: false, error: "method_not_allowed" }, 405);

  if (url.pathname === "/admin/stats") {
    const totalTables = [
      "raw_events",
      "messages",
      "download_jobs",
      "webhook_checks",
      "chatroom_snapshots",
      "chatroom_member_events",
      "chatroom_system_events"
    ];
    const totals = [];
    for (const table of totalTables) {
      const row = await env.DB.prepare(`SELECT COUNT(*) AS count FROM ${table}`).first();
      totals.push({ name: table, count: Number(row?.count || 0) });
    }
    const recent = await env.DB.prepare(`
      SELECT schema_version, msg_type, COUNT(*) AS count
      FROM raw_events
      WHERE received_at >= datetime('now', '-24 hours')
      GROUP BY schema_version, msg_type
      ORDER BY count DESC
      LIMIT 20
    `).all();
    const downloadJobStatuses = await env.DB.prepare(`
      SELECT status, asset_type, COUNT(*) AS count, MIN(created_at) AS oldest_created_at,
             MAX(updated_at) AS newest_updated_at
      FROM download_jobs
      GROUP BY status, asset_type
      ORDER BY status, asset_type
    `).all();
    return json({
      ok: true,
      totals,
      recent_24h: recent.results || [],
      download_job_statuses: downloadJobStatuses.results || []
    });
  }

  if (url.pathname === "/admin/download-jobs") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 500, 100);
    const afterJobId = clampInt(url.searchParams.get("after_job_id"), 0, Number.MAX_SAFE_INTEGER, 0);
    const status = url.searchParams.get("status");
    const assetType = normalizeAssetType(url.searchParams.get("asset_type"));
    const params = [afterJobId];
    let where = "d.id > ?";
    if (status) {
      where += " AND d.status = ?";
      params.push(status);
    }
    if (assetType) {
      where += " AND d.asset_type = ?";
      params.push(assetType);
    }
    const rows = await env.DB.prepare(`
      SELECT d.id AS job_id, d.job_key, m.message_key, d.message_id, d.raw_event_id,
             d.appid, d.account_wxid, d.asset_type, d.variant, d.endpoint, d.status,
             d.attempts, d.created_at, d.claimed_at, d.locked_until, d.next_attempt_at,
             d.completed_at, d.terminal_at, d.updated_at, d.source_url, d.size_bytes,
             d.mime_type, d.last_error
      FROM download_jobs d
      LEFT JOIN messages m ON m.id = d.message_id
      WHERE ${where}
      ORDER BY d.id ASC
      LIMIT ?
    `).bind(...params, limit).all();
    const jobs = rows.results || [];
    return json({
      ok: true,
      jobs,
      next_after_job_id: jobs.length ? jobs[jobs.length - 1].job_id : afterJobId
    });
  }

  if (url.pathname === "/admin/samples") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 100, 20);
    const result = await env.DB.prepare(`
      SELECT id, received_at, schema_version, appid, account_wxid, type_name, msg_id, new_msg_id,
             msg_type, from_user, to_user, conversation_id, sender_wxid, is_group, is_outgoing,
             wechat_created_at, content_text, push_content, duplicate_count
      FROM messages
      ORDER BY id DESC
      LIMIT ?
    `).bind(limit).all();
    return json({ ok: true, samples: result.results || [] });
  }

  if (url.pathname === "/admin/export") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 500, 100);
    const afterRawEventId = clampInt(url.searchParams.get("after_raw_event_id"), 0, Number.MAX_SAFE_INTEGER, 0);
    const rows = await env.DB.prepare(`
      SELECT id, received_at, schema_version, appid, account_wxid, type_name, msg_id, new_msg_id,
             msg_type, dedupe_key, event_json
      FROM raw_events
      WHERE id > ?
      ORDER BY id ASC
      LIMIT ?
    `).bind(afterRawEventId, limit).all();
    const events = (rows.results || []).map((row) => ({
      raw_event_id: row.id,
      received_at: row.received_at,
      schema_version: row.schema_version,
      appid: row.appid,
      account_wxid: row.account_wxid,
      type_name: row.type_name,
      msg_id: row.msg_id,
      new_msg_id: row.new_msg_id,
      msg_type: row.msg_type,
      dedupe_key: row.dedupe_key,
      body: parseStoredEventBody(row.event_json)
    }));
    return json({
      ok: true,
      events,
      next_after_raw_event_id: events.length ? events[events.length - 1].raw_event_id : afterRawEventId
    });
  }

  if (url.pathname === "/admin/attachments") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 500, 100);
    const afterJobId = clampInt(url.searchParams.get("after_job_id"), 0, Number.MAX_SAFE_INTEGER, 0);
    const rows = await env.DB.prepare(`
      SELECT d.id AS job_id, d.job_key, m.message_key, r.dedupe_key AS raw_event_dedupe_key,
             d.message_id, d.raw_event_id, d.appid, d.account_wxid, d.asset_type, d.variant,
             COALESCE(d.r2_object_key, d.local_path) AS object_key,
             d.size_bytes, d.mime_type, d.completed_at, d.created_at, d.updated_at
      FROM download_jobs d
      LEFT JOIN messages m ON m.id = d.message_id
      LEFT JOIN raw_events r ON r.id = d.raw_event_id
      WHERE d.id > ?
        AND d.status = 'completed'
        AND COALESCE(d.r2_object_key, d.local_path) IS NOT NULL
      ORDER BY d.id ASC
      LIMIT ?
    `).bind(afterJobId, limit).all();
    const attachments = (rows.results || []).map((row) => ({
      job_id: row.job_id,
      job_key: row.job_key,
      message_key: row.message_key,
      raw_event_dedupe_key: row.raw_event_dedupe_key,
      message_id: row.message_id,
      raw_event_id: row.raw_event_id,
      appid: row.appid,
      account_wxid: row.account_wxid,
      asset_type: row.asset_type,
      variant: row.variant,
      object_key: row.object_key,
      size_bytes: row.size_bytes,
      mime_type: row.mime_type,
      completed_at: row.completed_at,
      created_at: row.created_at,
      updated_at: row.updated_at,
      download_path: `/admin/attachment?job_id=${encodeURIComponent(row.job_id)}`
    }));
    return json({
      ok: true,
      attachments,
      next_after_job_id: attachments.length ? attachments[attachments.length - 1].job_id : afterJobId
    });
  }

  if (url.pathname === "/admin/attachment") {
    const jobId = Number(url.searchParams.get("job_id"));
    if (!Number.isInteger(jobId) || jobId <= 0) return json({ ok: false, error: "missing_job_id" }, 400);
    const row = await env.DB.prepare(`
      SELECT id, job_key, asset_type, mime_type, size_bytes, COALESCE(r2_object_key, local_path) AS object_key
      FROM download_jobs
      WHERE id = ? AND status = 'completed'
      LIMIT 1
    `).bind(jobId).first();
    if (!row?.object_key) return json({ ok: false, error: "attachment_not_found" }, 404);

    const object = await env.RAW_BUCKET.get(row.object_key);
    if (!object?.body) return json({ ok: false, error: "r2_object_not_found" }, 404);

    return new Response(object.body, {
      headers: compactHeaders({
        "content-type": row.mime_type || object.httpMetadata?.contentType || "application/octet-stream",
        "content-length": row.size_bytes ? String(row.size_bytes) : null,
        "x-gewe-skill-job-id": String(row.id),
        "x-gewe-skill-job-key": row.job_key || null,
        "x-gewe-skill-asset-type": row.asset_type || null
      })
    });
  }

  if (url.pathname === "/admin/chatroom-snapshots") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 100, 20);
    const chatroomId = url.searchParams.get("chatroom_id");
    const rows = chatroomId
      ? await env.DB.prepare(`
          SELECT id, received_at, chatroom_id, chatroom_name, chatroom_version, member_count, member_hash, members_json
          FROM chatroom_snapshots
          WHERE chatroom_id = ?
          ORDER BY id DESC
          LIMIT ?
        `).bind(chatroomId, limit).all()
      : await env.DB.prepare(`
          SELECT id, received_at, chatroom_id, chatroom_name, chatroom_version, member_count, member_hash, members_json
          FROM chatroom_snapshots
          ORDER BY id DESC
          LIMIT ?
        `).bind(limit).all();
    return json({ ok: true, snapshots: rows.results || [] });
  }

  if (url.pathname === "/admin/chatroom-events") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 500, 20);
    const chatroomId = url.searchParams.get("chatroom_id");
    const hasAfterId = url.searchParams.has("after_id");
    const afterId = Math.max(0, Number.parseInt(url.searchParams.get("after_id") || "0", 10) || 0);
    const rows = hasAfterId
      ? chatroomId
        ? await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, member_wxid, previous_chatroom_name, current_chatroom_name,
                 previous_member_count, current_member_count, previous_snapshot_id, current_snapshot_id, details_json
          FROM chatroom_member_events
          WHERE chatroom_id = ? AND id > ?
          ORDER BY id ASC
          LIMIT ?
        `).bind(chatroomId, afterId, limit).all()
        : await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, member_wxid, previous_chatroom_name, current_chatroom_name,
                 previous_member_count, current_member_count, previous_snapshot_id, current_snapshot_id, details_json
          FROM chatroom_member_events
          WHERE id > ?
          ORDER BY id ASC
          LIMIT ?
        `).bind(afterId, limit).all()
      : chatroomId
        ? await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, member_wxid, previous_chatroom_name, current_chatroom_name,
                 previous_member_count, current_member_count, previous_snapshot_id, current_snapshot_id, details_json
          FROM chatroom_member_events
          WHERE chatroom_id = ?
          ORDER BY id DESC
          LIMIT ?
        `).bind(chatroomId, limit).all()
      : await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, member_wxid, previous_chatroom_name, current_chatroom_name,
                 previous_member_count, current_member_count, previous_snapshot_id, current_snapshot_id, details_json
          FROM chatroom_member_events
          ORDER BY id DESC
          LIMIT ?
        `).bind(limit).all();
    const events = rows.results || [];
    const nextAfterEventId = events.reduce((max, row) => Math.max(max, Number(row.id) || max), afterId);
    return json({ ok: true, events, next_after_event_id: nextAfterEventId });
  }

  if (url.pathname === "/admin/chatroom-system-events") {
    const limit = clampInt(url.searchParams.get("limit"), 1, 500, 20);
    const chatroomId = url.searchParams.get("chatroom_id");
    const hasAfterId = url.searchParams.has("after_id");
    const afterId = Math.max(0, Number.parseInt(url.searchParams.get("after_id") || "0", 10) || 0);
    const rows = hasAfterId
      ? chatroomId
        ? await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, actor_wxid, actor_name,
                 target_wxid, target_name, target_wxids_json, target_names_json,
                 previous_value, current_value, template_text, content_text,
                 raw_event_id, message_id, details_json
          FROM chatroom_system_events
          WHERE chatroom_id = ? AND id > ?
          ORDER BY id ASC
          LIMIT ?
        `).bind(chatroomId, afterId, limit).all()
        : await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, actor_wxid, actor_name,
                 target_wxid, target_name, target_wxids_json, target_names_json,
                 previous_value, current_value, template_text, content_text,
                 raw_event_id, message_id, details_json
          FROM chatroom_system_events
          WHERE id > ?
          ORDER BY id ASC
          LIMIT ?
        `).bind(afterId, limit).all()
      : chatroomId
        ? await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, actor_wxid, actor_name,
                 target_wxid, target_name, target_wxids_json, target_names_json,
                 previous_value, current_value, template_text, content_text,
                 raw_event_id, message_id, details_json
          FROM chatroom_system_events
          WHERE chatroom_id = ?
          ORDER BY id DESC
          LIMIT ?
        `).bind(chatroomId, limit).all()
      : await env.DB.prepare(`
          SELECT id, received_at, event_type, chatroom_id, actor_wxid, actor_name,
                 target_wxid, target_name, target_wxids_json, target_names_json,
                 previous_value, current_value, template_text, content_text,
                 raw_event_id, message_id, details_json
          FROM chatroom_system_events
          ORDER BY id DESC
          LIMIT ?
        `).bind(limit).all();
    const events = rows.results || [];
    const nextAfterEventId = events.reduce((max, row) => Math.max(max, Number(row.id) || max), afterId);
    return json({ ok: true, events, next_after_event_id: nextAfterEventId });
  }

  if (url.pathname === "/admin/raw") {
    const id = Number(url.searchParams.get("id"));
    if (!Number.isInteger(id) || id <= 0) return json({ ok: false, error: "missing_id" }, 400);
    const row = await env.DB.prepare(`
      SELECT id, received_at, schema_version, appid, account_wxid, type_name, msg_id, new_msg_id,
             msg_type, dedupe_key, raw_object_key, event_json
      FROM raw_events
      WHERE id = ?
    `).bind(id).first();
    if (!row) return json({ ok: false, error: "not_found" }, 404);
    return json({ ok: true, raw: row });
  }

  return json({ ok: false, error: "not_found" }, 404);
}

async function retryDownloadJobs(env, url) {
  const limit = clampInt(url.searchParams.get("limit"), 1, 100, 20);
  const status = url.searchParams.get("status") || "failed";
  const assetType = normalizeAssetType(url.searchParams.get("asset_type"));
  const params = [status];
  let where = "status = ?";

  if (assetType) {
    where += " AND asset_type = ?";
    params.push(assetType);
  }

  const rows = await env.DB.prepare(`
    SELECT job_key
    FROM download_jobs
    WHERE ${where}
    ORDER BY COALESCE(updated_at, created_at) DESC
    LIMIT ?
  `).bind(...params, limit).all();

  const now = new Date().toISOString();
  const retried = [];
  for (const row of rows.results || []) {
    await env.DB.prepare(`
      UPDATE download_jobs
      SET status = 'pending', last_error = NULL, next_attempt_at = NULL,
          locked_until = NULL, terminal_at = NULL, attempts = 0,
          claimed_at = NULL, updated_at = ?
      WHERE job_key = ?
    `).bind(now, row.job_key).run();
    await env.ATTACHMENT_QUEUE.send({ jobKey: row.job_key });
    retried.push(row.job_key);
  }

  return json({ ok: true, retried_count: retried.length, retried });
}

async function handleAdminBackfillDownloadJobs(env, url) {
  const limit = clampInt(url.searchParams.get("limit"), 1, 500, 100);
  const assetType = normalizeAssetType(url.searchParams.get("asset_type"));
  const schemaVersion = url.searchParams.get("schema_version");
  const includeExisting = ["1", "true", "yes"].includes(String(url.searchParams.get("include_existing") || "").toLowerCase());
  const params = [];
  let where = "m.content_xml IS NOT NULL AND m.content_xml LIKE '<%'";

  if (schemaVersion) {
    where += " AND m.schema_version = ?";
    params.push(schemaVersion);
  }
  if (assetType) {
    where += ` AND ${assetTypeMessageWhere(assetType)}`;
  } else {
    where += ` AND (${DOWNLOAD_ASSET_TYPES.map(assetTypeMessageWhere).join(" OR ")})`;
  }
  if (!includeExisting) {
    if (assetType) {
      where += " AND NOT EXISTS (SELECT 1 FROM download_jobs d WHERE d.message_id = m.id AND d.asset_type = ?)";
      params.push(assetType);
    } else {
      where += " AND NOT EXISTS (SELECT 1 FROM download_jobs d WHERE d.message_id = m.id)";
    }
  }

  const rows = await env.DB.prepare(`
    SELECT m.id AS message_id, m.message_key, m.raw_event_id, m.appid, m.account_wxid,
           m.type_name, m.msg_id, m.new_msg_id, m.msg_type, m.appmsg_type,
           m.content_text, m.content_xml, m.schema_version
    FROM messages m
    WHERE ${where}
    ORDER BY m.id DESC
    LIMIT ?
  `).bind(...params, limit).all();

  const queued = [];
  const skipped = [];
  for (const row of rows.results || []) {
    const normalized = normalizedFromMessageRow(row);
    const job = buildDownloadJob(normalized, row.raw_event_id, row.message_id, env);
    if (!job) {
      skipped.push({ message_key: row.message_key, reason: "not_downloadable" });
      continue;
    }
    if (assetType && job.assetType !== assetType) {
      skipped.push({ message_key: row.message_key, reason: "asset_type_mismatch", asset_type: job.assetType });
      continue;
    }
    await enqueueDownloadJob(env, job);
    queued.push({ message_key: row.message_key, job_key: job.jobKey, asset_type: job.assetType });
  }

  return json({
    ok: true,
    scanned_count: rows.results?.length || 0,
    queued_count: queued.length,
    skipped_count: skipped.length,
    queued,
    skipped
  });
}

function normalizedFromMessageRow(row) {
  return {
    schemaVersion: row.schema_version,
    appid: row.appid,
    accountWxid: row.account_wxid,
    typeName: row.type_name,
    msgId: row.msg_id,
    newMsgId: row.new_msg_id,
    msgType: row.type_name || row.msg_type,
    appmsgType: row.appmsg_type,
    contentText: row.content_text,
    contentXml: row.content_xml,
    rawContent: row.content_xml || row.content_text,
    dedupeKey: row.message_key
  };
}

function normalizeAssetType(value) {
  const normalized = String(value || "").trim().toLowerCase();
  return DOWNLOAD_ASSET_TYPES.includes(normalized) ? normalized : null;
}

function assetTypeMessageWhere(assetType) {
  const map = {
    image: ["IMAGE", "3"],
    voice: ["VOICE", "34"],
    video: ["VIDEO", "43", "62"],
    emoji: ["EMOJI", "47"],
    file: ["FILE", "49"]
  };
  const values = map[assetType] || [];
  return `upper(CAST(COALESCE(m.type_name, m.msg_type, '') AS TEXT)) IN (${values.map((value) => `'${value}'`).join(", ")})`;
}

async function redactStoredRawEvents(env, url) {
  const limit = clampInt(url.searchParams.get("limit"), 1, 200, 50);
  const rows = await env.DB.prepare(`
    SELECT id, event_json, raw_object_key
    FROM raw_events
    WHERE (event_json LIKE '%"token":%' AND event_json NOT LIKE '%"token":"[redacted]"%')
       OR (event_json LIKE '%"tokenId":%' AND event_json NOT LIKE '%"tokenId":"[redacted]"%')
       OR (event_json LIKE '%"tokenName":%' AND event_json NOT LIKE '%"tokenName":"[redacted]"%')
       OR (event_json LIKE '%"userId":%' AND event_json NOT LIKE '%"userId":"[redacted]"%')
    ORDER BY id
    LIMIT ?
  `).bind(limit).all();

  const redactedIds = [];
  for (const row of rows.results || []) {
    const redacted = redactSensitiveBody(row.event_json);
    if (redacted === row.event_json) continue;

    await env.DB.prepare(`
      UPDATE raw_events
      SET event_json = ?, body_sha256 = ?
      WHERE id = ?
    `).bind(redacted, await sha256Hex(redacted), row.id).run();

    if (row.raw_object_key) {
      await env.RAW_BUCKET.put(row.raw_object_key, redacted, {
        httpMetadata: { contentType: "application/json; charset=utf-8" }
      });
    }

    redactedIds.push(row.id);
  }

  return json({ ok: true, scanned_count: rows.results?.length || 0, redacted_count: redactedIds.length, redacted_ids: redactedIds });
}

async function requireAdmin(request, env) {
  const authorization = request.headers.get("authorization") || "";
  const token = authorization.replace(/^Bearer\s+/i, "");
  if (!token || !env.ADMIN_API_KEY || !(await timingSafeEqual(token, env.ADMIN_API_KEY))) {
    return json({ ok: false, error: "unauthorized" }, 401);
  }
  return null;
}

function normalizeCallback(body, receivedAt) {
  if (body && typeof body === "object" && body.Data && (body.Appid || body.Wxid || body.TypeName)) {
    return normalizeV1(body, receivedAt);
  }
  if (body && typeof body === "object" && (body.appid || body.wxid || body.msgType)) {
    return normalizeV2(body, receivedAt);
  }
  return { schemaVersion: "unknown", dedupeKey: null, appid: null, receivedAt };
}

function normalizeV1(body, receivedAt) {
  const data = body.Data || {};
  const appid = scalar(body.Appid || body.appid);
  const accountWxid = scalar(body.Wxid || body.wxid);
  const rawTypeName = scalar(body.TypeName) || "v1";
  const msgId = scalar(unwrap(data.MsgId));
  const newMsgId = scalar(unwrap(data.NewMsgId));
  const rawMsgType = scalar(unwrap(data.MsgType));
  const contactUserName = scalar(unwrap(data.userName));
  const chatroomSnapshot = parseV1ChatroomSnapshot(data);
  const fromUserRaw = scalar(unwrap(data.FromUserName));
  const toUserRaw = scalar(unwrap(data.ToUserName));
  const fromUser = fromUserRaw || (rawTypeName === "ModContacts" ? contactUserName : null);
  const toUser = toUserRaw || (rawTypeName === "ModContacts" ? accountWxid : null);
  const rawContent = scalar(unwrap(data.Content));
  const pushContent = scalar(unwrap(data.PushContent));
  const msgSource = scalar(unwrap(data.MsgSource));
  const createTime = intOrNull(unwrap(data.CreateTime));
  const appmsgType = parseAppMsgType(rawContent);
  const semanticMsgType = normalizeV1SemanticType({ rawTypeName, rawMsgType, appmsgType, data, rawContent });
  const group = extractGroupSpeaker(fromUser, rawContent);
  const content = group.content || rawContent;
  const isOutgoing = Boolean(accountWxid && fromUserRaw === accountWxid);
  const conversationId = chooseV1ConversationId({ rawTypeName, contactUserName, fromUser, toUser, accountWxid, isOutgoing });
  const isGroup = isGroupConversation(conversationId) || isGroupConversation(fromUser) || isGroupConversation(toUser) || isGroupConversation(contactUserName);
  const contentText = normalizeV1ContentText({ rawTypeName, data, content });
  const identity = newMsgId || msgId || `${Date.parse(receivedAt)}:${awaitlessHashInput(body)}`;
  const dedupeKey = stableDedupeKey("v1", appid, identity);

  return {
    schemaVersion: "v1",
    appid,
    accountWxid,
    typeName: semanticMsgType,
    msgId,
    newMsgId,
    msgType: semanticMsgType,
    appmsgType,
    fromUser,
    toUser,
    conversationId,
    senderWxid: group.senderWxid || (isOutgoing ? accountWxid : (isGroupConversation(fromUser) ? null : fromUser)),
    isGroup,
    isOutgoing,
    wechatCreatedAt: createTime,
    contentText,
    contentXml: isLikelyXml(content) ? content : null,
    rawContent,
    pushContent: preview(pushContent),
    msgSource,
    dedupeKey,
    chatroomSnapshot
  };
}

function normalizeV1SemanticType({ rawTypeName, rawMsgType, appmsgType, data, rawContent }) {
  if (rawTypeName === "ModContacts") {
    const userName = scalar(unwrap(data?.userName));
    return isGroupConversation(userName) ? "CHATROOM_CONTACTS_UPDATE" : "CONTACTS_UPDATE";
  }

  if (rawTypeName !== "AddMsg") return toUpperSnake(rawTypeName);

  const type = String(rawMsgType || "");
  const baseTypes = {
    "1": "TEXT",
    "3": "IMAGE",
    "34": "VOICE",
    "42": "CONTACT_CARD",
    "43": "VIDEO",
    "47": "EMOJI",
    "48": "LOCATION",
    "51": "STATUS",
    "62": "VIDEO",
    "10000": "SYSTEM",
    "10002": "SYSTEM"
  };

  if (type === "49") return normalizeV1AppMsgType(appmsgType, rawContent);
  return baseTypes[type] || `MSG_${type || "UNKNOWN"}`;
}

function normalizeV1AppMsgType(appmsgType, rawContent) {
  if (appmsgType === 5) return "LINK";
  if (appmsgType === 6) return hasDownloadableFileXml(rawContent) ? "FILE" : "FILE_NOTICE";
  if (appmsgType === 19) return "CHAT_RECORD";
  if (appmsgType === 33 || appmsgType === 36) return "MINI_PROGRAM";
  if (appmsgType === 57) return "QUOTE";
  if (appmsgType === 74) return "FILE_NOTICE";
  return "APP_MSG";
}

function chooseV1ConversationId({ rawTypeName, contactUserName, fromUser, toUser, accountWxid, isOutgoing }) {
  if (rawTypeName === "ModContacts" && contactUserName) return contactUserName;
  return chooseConversationId({ fromUser, toUser, accountWxid, isOutgoing });
}

function normalizeV1ContentText({ rawTypeName, data, content }) {
  if (rawTypeName === "ModContacts") return summarizeV1ContactUpdate(data);
  return isLikelyXml(content) ? null : preview(content);
}

function summarizeV1ContactUpdate(data) {
  const userName = scalar(unwrap(data?.userName));
  const nickName = scalar(unwrap(data?.nickName));
  const chatroomVersion = scalar(unwrap(data?.chatroomVersion));
  const memberCount = intOrNull(data?.newChatroomData?.MemberCount);

  if (isGroupConversation(userName)) {
    const parts = [`chatroom snapshot: ${nickName || userName}`];
    if (memberCount !== null) parts.push(`members=${memberCount}`);
    if (chatroomVersion) parts.push(`version=${chatroomVersion}`);
    return preview(parts.join(", "));
  }

  return preview(`contact update: ${nickName || userName || "unknown"}`);
}

function parseV1ChatroomSnapshot(data) {
  const chatroomId = scalar(unwrap(data?.userName));
  if (!isGroupConversation(chatroomId)) return null;

  const chatroomName = scalar(unwrap(data?.nickName));
  const chatroomVersion = intOrNull(data?.chatroomVersion);
  const rawMembers = ensureArray(data?.newChatroomData?.ChatRoomMember);
  const members = rawMembers
    .map((member) => ({
      wxid: scalar(unwrap(member?.UserName)),
      flag: intOrNull(member?.ChatroomMemberFlag),
      displayName: scalar(unwrap(member?.DisplayName)) || scalar(unwrap(member?.NickName))
    }))
    .filter((member) => member.wxid)
    .sort((left, right) => left.wxid.localeCompare(right.wxid));
  const parsedMemberCount = intOrNull(data?.newChatroomData?.MemberCount);
  if (!members.length && parsedMemberCount === null) return null;
  const memberCount = parsedMemberCount ?? members.length;

  return {
    chatroomId,
    chatroomName,
    chatroomVersion,
    memberCount,
    members
  };
}

async function processChatroomSnapshot(env, normalized, rawEventId, messageId, receivedAt) {
  const snapshot = normalized.chatroomSnapshot;
  if (!snapshot || normalized.typeName !== "CHATROOM_CONTACTS_UPDATE") return;

  const previous = await env.DB.prepare(`
    SELECT id, chatroom_name, chatroom_version, member_count, members_json, member_hash
    FROM chatroom_snapshots
    WHERE chatroom_id = ?
    ORDER BY received_at DESC, id DESC
    LIMIT 1
  `).bind(snapshot.chatroomId).first();

  const membersJson = JSON.stringify(snapshot.members);
  const memberHash = await sha256Hex(membersJson);

  await env.DB.prepare(`
    INSERT INTO chatroom_snapshots (
      raw_event_id, message_id, appid, account_wxid, chatroom_id, chatroom_name,
      chatroom_version, member_count, members_json, member_hash, received_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(raw_event_id) DO NOTHING
  `).bind(
    rawEventId,
    messageId,
    normalized.appid,
    normalized.accountWxid,
    snapshot.chatroomId,
    snapshot.chatroomName,
    snapshot.chatroomVersion,
    snapshot.memberCount,
    membersJson,
    memberHash,
    receivedAt
  ).run();

  const current = await env.DB.prepare(`
    SELECT id, chatroom_name, chatroom_version, member_count, members_json, member_hash
    FROM chatroom_snapshots
    WHERE raw_event_id = ?
  `).bind(rawEventId).first();
  if (!current || !previous) return;

  await insertChatroomDiffEvents(env, {
    appid: normalized.appid,
    accountWxid: normalized.accountWxid,
    chatroomId: snapshot.chatroomId,
    previous,
    current,
    rawEventId,
    messageId,
    receivedAt
  });
}

async function insertChatroomDiffEvents(env, context) {
  const previousMembers = parseMembersJson(context.previous.members_json);
  const currentMembers = parseMembersJson(context.current.members_json);
  const previousByWxid = new Map(previousMembers.map((member) => [member.wxid, member]));
  const currentByWxid = new Map(currentMembers.map((member) => [member.wxid, member]));

  for (const member of currentMembers) {
    if (!previousByWxid.has(member.wxid)) {
      await insertChatroomEvent(env, context, {
        eventType: "member_joined",
        memberWxid: member.wxid,
        details: { member }
      });
    }
  }

  for (const member of previousMembers) {
    if (!currentByWxid.has(member.wxid)) {
      await insertChatroomEvent(env, context, {
        eventType: "member_left",
        memberWxid: member.wxid,
        details: { member }
      });
    }
  }

  if ((context.previous.chatroom_name || null) !== (context.current.chatroom_name || null)) {
    await insertChatroomEvent(env, context, {
      eventType: "chatroom_name_changed",
      details: {
        previous_name: context.previous.chatroom_name,
        current_name: context.current.chatroom_name
      }
    });
  }

  if (Number(context.previous.member_count) !== Number(context.current.member_count)) {
    await insertChatroomEvent(env, context, {
      eventType: "member_count_changed",
      details: {
        previous_count: context.previous.member_count,
        current_count: context.current.member_count
      }
    });
  }
}

async function insertChatroomEvent(env, context, event) {
  const eventKey = [
    context.chatroomId,
    context.current.id,
    event.eventType,
    event.memberWxid || event.details?.current_name || event.details?.current_count || "chatroom"
  ].join(":");

  await env.DB.prepare(`
    INSERT INTO chatroom_member_events (
      event_key, event_type, appid, account_wxid, chatroom_id, member_wxid,
      previous_chatroom_name, current_chatroom_name, previous_member_count, current_member_count,
      previous_snapshot_id, current_snapshot_id, raw_event_id, message_id, received_at, details_json
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(event_key) DO NOTHING
  `).bind(
    eventKey,
    event.eventType,
    context.appid,
    context.accountWxid,
    context.chatroomId,
    event.memberWxid || null,
    context.previous.chatroom_name,
    context.current.chatroom_name,
    context.previous.member_count,
    context.current.member_count,
    context.previous.id,
    context.current.id,
    context.rawEventId,
    context.messageId,
    context.receivedAt,
    JSON.stringify(event.details || {})
  ).run();
}

function parseMembersJson(text) {
  try {
    const value = JSON.parse(text || "[]");
    return Array.isArray(value) ? value.filter((member) => member?.wxid) : [];
  } catch (_error) {
    return [];
  }
}

async function processChatroomSystemEvent(env, normalized, rawEventId, messageId, receivedAt) {
  const systemEvent = parseChatroomSystemEvent(normalized, rawEventId, messageId, receivedAt);
  if (!systemEvent) return;
  await insertChatroomSystemEvent(env, systemEvent);
}

function parseChatroomSystemEvent(normalized, rawEventId, messageId, receivedAt) {
  if (!normalized) return null;

  const typeName = String(normalized.typeName || normalized.type_name || "").toUpperCase();
  const msgType = Number(normalized.msgType || normalized.msg_type || 0);
  if (typeName !== "SYSTEM" && msgType !== 10000 && msgType !== 10002) return null;

  const chatroomId = normalized.conversationId || normalized.conversation_id || normalized.fromUser || normalized.from_user || "";
  if (!isGroupConversation(chatroomId)) return null;

  const xml = String(normalized.contentXml || normalized.content_xml || normalized.contentText || normalized.content_text || normalized.rawContent || "");
  if (!/<sysmsg\b/i.test(xml)) return null;

  const templateText = extractSysmsgTemplate(xml);
  const links = extractSysmsgLinks(xml);
  const names = links.names || [];
  const kickoutNames = links.kickoutname || [];
  const username = firstLinkMember(links.username);
  const remark = firstLinkMember(links.remark);
  const template = templateText || "";

  let eventType = "system_unknown";
  let targetMembers = [];
  if ((template.includes("加入了群聊") || template.includes("邀请")) && names.length > 0) {
    eventType = "member_invited";
    targetMembers = names;
  } else if (template.includes("移出")) {
    eventType = "member_removed";
    targetMembers = kickoutNames.length > 0 ? kickoutNames : names;
  } else if (template.includes("退出群聊")) {
    eventType = "member_left";
  } else if (template.includes("修改群名") || template.includes("群名")) {
    eventType = "chatroom_name_changed";
  }

  const targetWxids = targetMembers.map((member) => member.username).filter(Boolean);
  const targetNames = targetMembers.map((member) => member.nickname || member.username).filter(Boolean);

  let actorWxid = username?.username || null;
  let actorName = username?.nickname || username?.username || null;
  if (!actorWxid && template.startsWith("你")) {
    actorWxid = normalized.accountWxid || normalized.account_wxid || null;
    actorName = "你";
  }

  const currentValue = eventType === "chatroom_name_changed"
    ? (remark?.nickname || remark?.username || null)
    : null;
  const contentText = renderSysmsgText(template, links) || String(normalized.contentText || normalized.content_text || "").slice(0, 500);

  return {
    rawEventId,
    messageId,
    appid: normalized.appid,
    accountWxid: normalized.accountWxid || normalized.account_wxid || null,
    chatroomId,
    eventType,
    actorWxid,
    actorName,
    targetWxid: targetWxids[0] || null,
    targetName: targetNames[0] || null,
    targetWxids,
    targetNames,
    previousValue: null,
    currentValue,
    templateText: template || null,
    contentText: contentText || null,
    receivedAt,
    details: { links }
  };
}

async function insertChatroomSystemEvent(env, event) {
  await env.DB.prepare(`
    INSERT INTO chatroom_system_events (
      raw_event_id, message_id, appid, account_wxid, chatroom_id, event_type,
      actor_wxid, actor_name, target_wxid, target_name, target_wxids_json, target_names_json,
      previous_value, current_value, template_text, content_text, received_at, details_json
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(raw_event_id) DO UPDATE SET
      message_id = excluded.message_id,
      appid = excluded.appid,
      account_wxid = excluded.account_wxid,
      chatroom_id = excluded.chatroom_id,
      event_type = excluded.event_type,
      actor_wxid = excluded.actor_wxid,
      actor_name = excluded.actor_name,
      target_wxid = excluded.target_wxid,
      target_name = excluded.target_name,
      target_wxids_json = excluded.target_wxids_json,
      target_names_json = excluded.target_names_json,
      previous_value = excluded.previous_value,
      current_value = excluded.current_value,
      template_text = excluded.template_text,
      content_text = excluded.content_text,
      received_at = excluded.received_at,
      details_json = excluded.details_json
  `).bind(
    event.rawEventId,
    event.messageId,
    event.appid,
    event.accountWxid,
    event.chatroomId,
    event.eventType,
    event.actorWxid,
    event.actorName,
    event.targetWxid,
    event.targetName,
    JSON.stringify(event.targetWxids || []),
    JSON.stringify(event.targetNames || []),
    event.previousValue,
    event.currentValue,
    event.templateText,
    event.contentText,
    event.receivedAt,
    JSON.stringify(event.details || {})
  ).run();
}

async function handleAdminBackfillChatroomEvents(request, env) {
  const url = new URL(request.url);
  const limit = clampInt(url.searchParams.get("limit"), 1, 5000, 2000);
  const snapshotResult = await backfillChatroomSnapshots(env, limit);
  const systemResult = await backfillChatroomSystemEvents(env, limit);
  const rebuiltResult = await rebuildChatroomMemberEvents(env, snapshotResult.chatroom_ids);

  return json({
    ok: true,
    snapshots: snapshotResult,
    member_events: rebuiltResult,
    system_events: systemResult
  });
}

async function backfillChatroomSnapshots(env, limit) {
  const rows = await env.DB.prepare(`
    SELECT r.id AS raw_event_id, COALESCE(m.id, 0) AS message_id, r.appid, r.account_wxid,
           r.received_at, r.event_json
    FROM raw_events r
    LEFT JOIN messages m ON m.raw_event_id = r.id
    WHERE r.schema_version = 'v1'
      AND (
        r.type_name IN ('CHATROOM_CONTACTS_UPDATE', 'ModContacts')
        OR m.type_name IN ('CHATROOM_CONTACTS_UPDATE', 'ModContacts')
        OR r.event_json LIKE '%"TypeName":"ModContacts"%'
        OR r.event_json LIKE '%"TypeName": "ModContacts"%'
      )
    ORDER BY r.received_at ASC, r.id ASC
    LIMIT ?
  `).bind(limit).all();

  let inserted = 0;
  let skipped = 0;
  const chatroomIds = new Set();
  for (const row of rows.results || []) {
    const body = parseStoredEventBody(row.event_json);
    const data = body?.Data || body?.data || null;
    const snapshot = data ? parseV1ChatroomSnapshot(data) : null;
    if (!snapshot) {
      skipped += 1;
      continue;
    }

    const membersJson = JSON.stringify(snapshot.members);
    const memberHash = await sha256Hex(membersJson);
    const appid = body.Appid || body.appid || row.appid || "";
    const accountWxid = body.Wxid || body.wxid || row.account_wxid || null;
    const result = await env.DB.prepare(`
      INSERT INTO chatroom_snapshots (
        raw_event_id, message_id, appid, account_wxid, chatroom_id, chatroom_name,
        chatroom_version, member_count, members_json, member_hash, received_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      ON CONFLICT(raw_event_id) DO NOTHING
    `).bind(
      row.raw_event_id,
      row.message_id,
      appid,
      accountWxid,
      snapshot.chatroomId,
      snapshot.chatroomName,
      snapshot.chatroomVersion,
      snapshot.memberCount,
      membersJson,
      memberHash,
      row.received_at
    ).run();
    if (result.meta?.changes) inserted += 1;
    chatroomIds.add(snapshot.chatroomId);
  }

  return {
    scanned: rows.results?.length || 0,
    inserted,
    skipped,
    chatroom_ids: [...chatroomIds]
  };
}

async function rebuildChatroomMemberEvents(env, chatroomIds) {
  let deleted = 0;
  let inserted = 0;
  const rebuiltChatrooms = [];

  for (const chatroomId of chatroomIds || []) {
    const deleteResult = await env.DB.prepare(`
      DELETE FROM chatroom_member_events
      WHERE chatroom_id = ?
    `).bind(chatroomId).run();
    deleted += deleteResult.meta?.changes || 0;

    const snapshots = await env.DB.prepare(`
      SELECT id, raw_event_id, message_id, appid, account_wxid, chatroom_id, chatroom_name,
             chatroom_version, member_count, members_json, member_hash, received_at
      FROM chatroom_snapshots
      WHERE chatroom_id = ?
      ORDER BY received_at ASC, id ASC
    `).bind(chatroomId).all();

    const snapshotRows = snapshots.results || [];
    for (let index = 1; index < snapshotRows.length; index += 1) {
      const previous = snapshotRows[index - 1];
      const current = snapshotRows[index];
      const before = inserted;
      await insertChatroomDiffEvents(env, {
        appid: current.appid || previous.appid,
        accountWxid: current.account_wxid || previous.account_wxid,
        chatroomId,
        previous,
        current,
        rawEventId: current.raw_event_id,
        messageId: current.message_id,
        receivedAt: current.received_at
      });

      const eventCount = await env.DB.prepare(`
        SELECT COUNT(*) AS count
        FROM chatroom_member_events
        WHERE current_snapshot_id = ?
      `).bind(current.id).first();
      inserted += Math.max(0, Number(eventCount?.count || 0) - (inserted - before));
    }

    rebuiltChatrooms.push({ chatroom_id: chatroomId, snapshots: snapshotRows.length });
  }

  const total = await env.DB.prepare(`
    SELECT COUNT(*) AS count
    FROM chatroom_member_events
    WHERE chatroom_id IN (${chatroomIds.map(() => "?").join(",") || "NULL"})
  `).bind(...chatroomIds).first();

  return {
    chatrooms: rebuiltChatrooms,
    deleted,
    inserted: Number(total?.count || 0)
  };
}

async function backfillChatroomSystemEvents(env, limit) {
  const rows = await env.DB.prepare(`
    SELECT r.id AS raw_event_id, COALESCE(m.id, 0) AS message_id, r.appid, r.account_wxid,
           r.received_at, r.event_json
    FROM raw_events r
    LEFT JOIN messages m ON m.raw_event_id = r.id
    WHERE r.schema_version = 'v1'
      AND (
        r.type_name = 'SYSTEM'
        OR m.type_name = 'SYSTEM'
        OR m.msg_type IN (10000, 10002)
        OR r.event_json LIKE '%"MsgType":10000%'
        OR r.event_json LIKE '%"MsgType":10002%'
      )
    ORDER BY r.received_at ASC, r.id ASC
    LIMIT ?
  `).bind(limit).all();

  let inserted = 0;
  let skipped = 0;
  for (const row of rows.results || []) {
    const body = parseStoredEventBody(row.event_json);
    if (!body) {
      skipped += 1;
      continue;
    }

    const normalized = normalizeStoredV1SystemEvent(body, row);
    const event = parseChatroomSystemEvent(normalized, row.raw_event_id, row.message_id, row.received_at);
    if (!event) {
      skipped += 1;
      continue;
    }

    const before = await env.DB.prepare(`SELECT id FROM chatroom_system_events WHERE raw_event_id = ?`).bind(row.raw_event_id).first();
    await insertChatroomSystemEvent(env, event);
    const after = await env.DB.prepare(`SELECT id FROM chatroom_system_events WHERE raw_event_id = ?`).bind(row.raw_event_id).first();
    if (!before && after) inserted += 1;
  }

  return {
    scanned: rows.results?.length || 0,
    inserted,
    skipped
  };
}

function parseStoredEventBody(text) {
  try {
    const value = JSON.parse(text || "{}");
    return value?.body || value;
  } catch (_error) {
    return null;
  }
}

function normalizeStoredV1SystemEvent(body, row) {
  try {
    return normalizeV1(body, row.received_at);
  } catch (_error) {
    const data = body?.Data || body?.data || {};
    const fromUser = scalar(unwrap(data.FromUserName || data.fromUserName || data.fromUser || data.from_user));
    const toUser = scalar(unwrap(data.ToUserName || data.toUserName || data.toUser || data.to_user));
    const rawContent = scalar(unwrap(data.Content || data.content));
    const contentXml = stripGroupSpeakerPrefix(rawContent);

    return {
      appid: body.Appid || body.appid || row.appid,
      accountWxid: body.Wxid || body.wxid || row.account_wxid,
      typeName: "SYSTEM",
      msgType: Number(data.MsgType || data.msgType || data.msg_type || 0),
      conversationId: isGroupConversation(fromUser) ? fromUser : toUser,
      fromUser,
      toUser,
      contentXml,
      contentText: rawContent
    };
  }
}

function stripGroupSpeakerPrefix(text) {
  const value = String(text || "");
  const match = value.match(/^[^:\n]+:\n([\s\S]*)$/);
  return match ? match[1] : value;
}

function extractSysmsgTemplate(xml) {
  const match = String(xml || "").match(/<template>([\s\S]*?)<\/template>/i);
  return match ? decodeXmlText(stripCdata(match[1])).trim() : "";
}

function extractSysmsgLinks(xml) {
  const output = {};
  const linkRegex = /<link\b[^>]*\bname=["']([^"']+)["'][^>]*>([\s\S]*?)<\/link>/gi;
  let linkMatch;
  while ((linkMatch = linkRegex.exec(String(xml || "")))) {
    const name = linkMatch[1];
    const body = linkMatch[2];
    const members = [];
    const memberRegex = /<member>([\s\S]*?)<\/member>/gi;
    let memberMatch;
    while ((memberMatch = memberRegex.exec(body))) {
      const username = decodeXmlText(stripCdata(extractXmlTag(memberMatch[1], "username"))).trim();
      const nickname = decodeXmlText(stripCdata(extractXmlTag(memberMatch[1], "nickname"))).trim();
      members.push({ username, nickname });
    }
    output[name] = members;
  }
  return output;
}

function firstLinkMember(members) {
  return Array.isArray(members) && members.length > 0 ? members[0] : null;
}

function renderSysmsgText(template, links) {
  let text = String(template || "");
  for (const [name, members] of Object.entries(links || {})) {
    const label = (members || []).map((member) => member.nickname || member.username).filter(Boolean).join(", ");
    text = text.replaceAll(`$${name}$`, label);
  }
  return text.trim();
}

function extractXmlTag(text, tagName) {
  const match = String(text || "").match(new RegExp(`<${tagName}>([\\s\\S]*?)<\\/${tagName}>`, "i"));
  return match ? match[1] : "";
}

function stripCdata(text) {
  return String(text || "").trim().replace(/^<!\[CDATA\[/, "").replace(/\]\]>$/, "");
}

function decodeXmlText(text) {
  return String(text || "")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&")
    .replace(/&#x([0-9a-f]+);/gi, (_match, hex) => String.fromCodePoint(Number.parseInt(hex, 16)))
    .replace(/&#(\d+);/g, (_match, number) => String.fromCodePoint(Number.parseInt(number, 10)));
}

function normalizeV2(body, receivedAt) {
  const appid = scalar(body.appid);
  const accountWxid = scalar(body.wxid);
  const msgId = scalar(body.msgId);
  const newMsgId = scalar(body.newMsgId);
  const msgType = scalar(body.msgType);
  const fromUser = scalar(body.fromUser);
  const toUser = scalar(body.toUser);
  const fromGroup = scalar(body.fromGroup);
  const rawContent = scalar(body.content);
  const createTime = intOrNull(body.createTime);
  const isOutgoing = body.isSelf === true;
  const appmsgType = parseAppMsgType(rawContent);
  const group = extractGroupSpeaker(fromUser, rawContent);
  const content = group.content || rawContent;
  const conversationId = fromGroup || chooseConversationId({ fromUser, toUser, accountWxid, isOutgoing });
  const isGroup = isGroupConversation(fromGroup) || isGroupConversation(conversationId) || body.eventCode === "group_msg_event";
  const identity = newMsgId || msgId || `${Date.parse(receivedAt)}:${awaitlessHashInput(body)}`;
  const dedupeKey = stableDedupeKey("v2", appid, identity);

  return {
    schemaVersion: "v2",
    appid,
    accountWxid,
    typeName: msgType || "v2",
    msgId,
    newMsgId,
    msgType,
    appmsgType,
    fromUser,
    toUser,
    conversationId,
    senderWxid: group.senderWxid || (isGroup ? fromUser : (isOutgoing ? accountWxid : fromUser)),
    isGroup,
    isOutgoing,
    wechatCreatedAt: createTime,
    contentText: isLikelyXml(content) ? null : preview(content),
    contentXml: isLikelyXml(content) ? content : null,
    rawContent,
    pushContent: null,
    msgSource: scalar(body.msgSource),
    dedupeKey
  };
}

async function upsertRawEvent(env, normalized, bodyText, bodySha256, rawObjectKey, sourceIp, userAgent, receivedAt) {
  await env.DB.prepare(`
    INSERT INTO raw_events (
      received_at, appid, account_wxid, type_name, msg_id, new_msg_id, msg_type,
      dedupe_key, body_sha256, event_json, source_ip, user_agent, raw_object_key, schema_version
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(dedupe_key) DO UPDATE SET
      duplicate_count = duplicate_count + 1,
      last_seen_at = excluded.received_at
  `).bind(
    receivedAt,
    normalized.appid,
    normalized.accountWxid,
    normalized.typeName,
    normalized.msgId,
    normalized.newMsgId,
    normalized.msgType,
    normalized.dedupeKey,
    bodySha256,
    bodyText,
    sourceIp,
    userAgent,
    rawObjectKey,
    normalized.schemaVersion
  ).run();

  return env.DB.prepare(`SELECT id, duplicate_count FROM raw_events WHERE dedupe_key = ?`).bind(normalized.dedupeKey).first();
}

async function upsertMessage(env, normalized, rawEventId, receivedAt) {
  const messageKey = normalized.dedupeKey;
  await env.DB.prepare(`
    INSERT INTO messages (
      message_key, raw_event_id, appid, account_wxid, type_name, msg_id, new_msg_id, msg_type,
      appmsg_type, from_user, to_user, conversation_id, sender_wxid, is_group, is_outgoing,
      wechat_created_at, received_at, last_seen_at, content_text, content_xml, push_content, msg_source, schema_version
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(message_key) DO UPDATE SET
      duplicate_count = duplicate_count + 1,
      last_seen_at = excluded.received_at
  `).bind(
    messageKey,
    rawEventId,
    normalized.appid,
    normalized.accountWxid,
    normalized.typeName || normalized.schemaVersion,
    normalized.msgId,
    normalized.newMsgId,
    normalized.msgType,
    normalized.appmsgType,
    normalized.fromUser,
    normalized.toUser,
    normalized.conversationId,
    normalized.senderWxid,
    normalized.isGroup ? 1 : 0,
    normalized.isOutgoing ? 1 : 0,
    normalized.wechatCreatedAt,
    receivedAt,
    receivedAt,
    normalized.contentText,
    normalized.contentXml,
    normalized.pushContent,
    normalized.msgSource,
    normalized.schemaVersion
  ).run();

  return env.DB.prepare(`SELECT id, duplicate_count FROM messages WHERE message_key = ?`).bind(messageKey).first();
}

function buildDownloadJob(normalized, rawEventId, messageId, env) {
  const kind = attachmentKind(normalized);
  const xml = downloadableXml(normalized);
  if (!kind || !xml) return null;

  const endpoint = downloadEndpoint(kind);
  if (!endpoint) return null;

  const requestJson = {
    appId: normalized.appid,
    xml
  };
  if (kind === "image") requestJson.type = clampInt(env.IMAGE_DOWNLOAD_TYPE, 1, 3, 2);
  if (kind === "voice" && normalized.msgId) requestJson.msgId = Number(normalized.msgId);

  return {
    jobKey: `${normalized.dedupeKey}:${kind}`,
    messageId,
    rawEventId,
    appid: normalized.appid,
    accountWxid: normalized.accountWxid,
    assetType: kind,
    variant: kind === "image" ? String(requestJson.type) : null,
    endpoint,
    requestJson
  };
}

function downloadableXml(normalized) {
  for (const value of [normalized.contentXml, normalized.rawContent, normalized.contentText]) {
    if (isLikelyXml(value)) return value;
  }
  for (const value of [normalized.rawContent, normalized.contentText]) {
    if (typeof value !== "string") continue;
    const index = ["<msg", "<appmsg"].map((marker) => value.indexOf(marker)).filter((item) => item >= 0).sort((left, right) => left - right)[0];
    if (index !== undefined) return value.slice(index);
  }
  return null;
}

async function enqueueDownloadJob(env, job) {
  const now = new Date().toISOString();
  await env.DB.prepare(`
    INSERT INTO download_jobs (
      job_key, message_id, raw_event_id, appid, account_wxid, asset_type,
      variant, endpoint, request_json, status, created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?)
    ON CONFLICT(job_key) DO UPDATE SET
      message_id = excluded.message_id,
      raw_event_id = excluded.raw_event_id,
      endpoint = excluded.endpoint,
      request_json = excluded.request_json,
      updated_at = CASE
        WHEN download_jobs.status IN ('completed', 'pending_large', 'pending_unknown_size', 'failed', 'unavailable', 'purged', 'skipped_not_file')
          THEN download_jobs.updated_at
        ELSE excluded.updated_at
      END
  `).bind(
    job.jobKey,
    job.messageId,
    job.rawEventId,
    job.appid,
    job.accountWxid,
    job.assetType,
    job.variant,
    job.endpoint,
    JSON.stringify(job.requestJson),
    now,
    now
  ).run();

  await env.ATTACHMENT_QUEUE.send({ jobKey: job.jobKey });
}

async function processDownloadJob(body, env) {
  const jobKey = body?.jobKey;
  if (!jobKey) return;
  const job = await claimDownloadJob(env, jobKey);
  if (!job) return;

  try {
    const apiResult = await callGeweDownload(env, job.endpoint, JSON.parse(job.request_json));
    const fileUrl = extractDownloadUrl(apiResult);
    if (!fileUrl) throw new Error(`Gewe download returned no downloadable URL: ${JSON.stringify(apiResult).slice(0, 500)}`);

    const maxBytes = clampInt(env.MAX_ATTACHMENT_BYTES, 1, 1024 * 1024 * 1024, 200 * 1024 * 1024);
    const head = await fetch(fileUrl, { method: "HEAD" }).catch(() => null);
    const headSizeBytes = Number(head?.headers.get("content-length") || 0);
    if (headSizeBytes > maxBytes) {
      await markDownloadJob(env, jobKey, "pending_large", `Remote file ${headSizeBytes} exceeds max ${maxBytes}`, {
        sourceUrl: fileUrl,
        sizeBytes: headSizeBytes,
        mimeType: normalizeDownloadedMime(job.asset_type, head?.headers.get("content-type") || null, fileUrl),
        terminal: true
      });
      return;
    }

    const fileResponse = await fetch(fileUrl);
    if (!fileResponse.ok || !fileResponse.body) throw new Error(`Remote file fetch failed: ${fileResponse.status}`);

    const responseSizeBytes = Number(fileResponse.headers.get("content-length") || headSizeBytes || 0);
    const objectKey = attachmentObjectKey(job.asset_type, jobKey, fileUrl, fileResponse.headers.get("content-type") || null);
    const mimeType = normalizeDownloadedMime(job.asset_type, fileResponse.headers.get("content-type") || head?.headers.get("content-type") || null, objectKey);
    if (!responseSizeBytes) {
      await markDownloadJob(env, jobKey, "pending_unknown_size", "Remote file GET response has no content-length", { sourceUrl: fileUrl, mimeType, terminal: true });
      return;
    }
    if (responseSizeBytes > maxBytes) {
      await markDownloadJob(env, jobKey, "pending_large", `Remote file ${responseSizeBytes} exceeds max ${maxBytes}`, { sourceUrl: fileUrl, sizeBytes: responseSizeBytes, mimeType, terminal: true });
      return;
    }

    await env.RAW_BUCKET.put(objectKey, fileResponse.body, {
      httpMetadata: { contentType: mimeType || undefined },
      customMetadata: compactMetadata({ appid: job.appid, wxid: job.account_wxid, job_key: jobKey, asset_type: job.asset_type })
    });

    await env.DB.prepare(`
      UPDATE download_jobs
      SET status = 'completed', completed_at = ?, updated_at = ?, local_path = ?, r2_object_key = ?,
          source_url = ?, size_bytes = ?, mime_type = ?, last_error = NULL,
          next_attempt_at = NULL, locked_until = NULL, terminal_at = NULL
      WHERE job_key = ?
    `).bind(new Date().toISOString(), new Date().toISOString(), objectKey, objectKey, fileUrl, responseSizeBytes, mimeType, jobKey).run();
  } catch (error) {
    await finishDownloadFailure(env, job, error);
  }
}

async function claimDownloadJob(env, jobKey) {
  const existing = await env.DB.prepare(`SELECT * FROM download_jobs WHERE job_key = ?`).bind(jobKey).first();
  if (!existing || isTerminalDownloadStatus(existing.status)) return null;

  const now = new Date();
  const nowIso = now.toISOString();
  if (existing.status === "retry_scheduled" && existing.next_attempt_at && existing.next_attempt_at > nowIso) return null;
  if (existing.status === "processing" && existing.locked_until && existing.locked_until > nowIso) return null;

  const lockedUntil = addSeconds(now, clampInt(env.DOWNLOAD_LOCK_SECONDS, 30, 3600, 300)).toISOString();
  const result = await env.DB.prepare(`
    UPDATE download_jobs
    SET status = 'processing', attempts = attempts + 1, claimed_at = ?,
        locked_until = ?, next_attempt_at = NULL, updated_at = ?
    WHERE job_key = ?
      AND status IN ('pending', 'retry_scheduled', 'processing')
      AND (next_attempt_at IS NULL OR next_attempt_at <= ?)
      AND (locked_until IS NULL OR locked_until <= ? OR status != 'processing')
  `).bind(nowIso, lockedUntil, nowIso, jobKey, nowIso, nowIso).run();
  if (!result.meta?.changes) return null;

  return env.DB.prepare(`SELECT * FROM download_jobs WHERE job_key = ?`).bind(jobKey).first();
}

async function finishDownloadFailure(env, job, error) {
  const message = safeError(error);
  const disposition = classifyDownloadError(message);
  const attempts = Number(job.attempts || 0);
  const maxAttempts = clampInt(env.DOWNLOAD_MAX_ATTEMPTS, 1, 50, 6);

  if (disposition.terminal) {
    await markDownloadJob(env, job.job_key, disposition.status, message, { terminal: true });
    return;
  }

  if (attempts >= maxAttempts) {
    await markDownloadJob(env, job.job_key, "failed", message, { terminal: true });
    return;
  }

  const delaySeconds = downloadRetryDelaySeconds(env, attempts);
  const nextAttemptAt = addSeconds(new Date(), delaySeconds).toISOString();
  await markDownloadJob(env, job.job_key, "retry_scheduled", message, { nextAttemptAt });
}

function isTerminalDownloadStatus(status) {
  return [
    "completed",
    "pending_large",
    "pending_unknown_size",
    "failed",
    "unavailable",
    "purged",
    "skipped_not_file"
  ].includes(String(status || ""));
}

function classifyDownloadError(message) {
  const text = String(message || "");
  if (/NullPointerException|no downloadable URL|not found|404|expired|文件不存在|资源不存在|已过期/i.test(text)) {
    return { terminal: true, status: "unavailable" };
  }
  if (/最大支持2条并发|请稍后再试|rate limit|too many requests|timeout|timed out|network|fetch failed|429|500|502|503|504/i.test(text)) {
    return { terminal: false, status: "retry_scheduled" };
  }
  return { terminal: false, status: "retry_scheduled" };
}

function downloadRetryDelaySeconds(env, attempts) {
  const base = clampInt(env.DOWNLOAD_RETRY_BASE_SECONDS, 5, 3600, 30);
  const max = clampInt(env.DOWNLOAD_RETRY_MAX_SECONDS, base, 24 * 3600, 1800);
  const exponent = Math.max(0, Math.min(10, Number(attempts || 1) - 1));
  const jitter = Math.floor(Math.random() * Math.min(base, 30));
  return Math.min(max, base * (2 ** exponent) + jitter);
}

async function callGeweDownload(env, endpoint, requestJson) {
  const response = await fetch(`${env.GEWE_API_BASE}${endpoint}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "X-GEWE-TOKEN": env.GEWE_TOKEN
    },
    body: JSON.stringify(requestJson)
  });
  const text = await response.text();
  let data;
  try {
    data = JSON.parse(text || "{}");
  } catch (_error) {
    throw new Error(`Gewe API returned non-JSON ${response.status}: ${text.slice(0, 300)}`);
  }
  if (!response.ok || data.ret !== 200) {
    throw new Error(`Gewe API error ${response.status}: ${JSON.stringify(data).slice(0, 500)}`);
  }
  return data;
}

function extractDownloadUrl(apiResult) {
  return apiResult?.data?.fileUrl || apiResult?.data?.url || null;
}

async function markDownloadJob(env, jobKey, status, error, extra = {}) {
  const now = new Date().toISOString();
  const terminalAt = extra.terminal ? now : null;
  await env.DB.prepare(`
    UPDATE download_jobs
    SET status = ?, last_error = ?, updated_at = ?, source_url = COALESCE(?, source_url),
        size_bytes = COALESCE(?, size_bytes), mime_type = COALESCE(?, mime_type),
        next_attempt_at = ?, locked_until = NULL, terminal_at = ?
    WHERE job_key = ?
  `).bind(
    status,
    String(error || "").slice(0, 1000),
    now,
    extra.sourceUrl || null,
    extra.sizeBytes || null,
    extra.mimeType || null,
    extra.nextAttemptAt || null,
    terminalAt,
    jobKey
  ).run();
}

async function scheduledMaintenance(env, cron) {
  await requeueDueDownloadJobs(env);
  if (cron === "37 18 * * *") {
    await cleanup(env);
  }
}

async function requeueDueDownloadJobs(env) {
  const limit = clampInt(env.DOWNLOAD_SWEEP_LIMIT, 1, 500, 100);
  const now = new Date().toISOString();
  const rows = await env.DB.prepare(`
    SELECT job_key, status
    FROM download_jobs
    WHERE status = 'pending'
       OR (status = 'retry_scheduled' AND (next_attempt_at IS NULL OR next_attempt_at <= ?))
       OR (status = 'processing' AND (locked_until IS NULL OR locked_until <= ?))
    ORDER BY COALESCE(next_attempt_at, updated_at, created_at) ASC
    LIMIT ?
  `).bind(now, now, limit).all();

  let requeued = 0;
  let recovered = 0;
  for (const row of rows.results || []) {
    if (row.status === "processing") {
      await env.DB.prepare(`
        UPDATE download_jobs
        SET status = 'pending', locked_until = NULL, next_attempt_at = NULL,
            last_error = COALESCE(last_error, 'stale processing lock recovered'),
            updated_at = ?
        WHERE job_key = ?
      `).bind(now, row.job_key).run();
      recovered += 1;
    }
    await env.ATTACHMENT_QUEUE.send({ jobKey: row.job_key });
    requeued += 1;
  }

  return { requeued_count: requeued, recovered_processing_count: recovered };
}

async function cleanup(env) {
  const attachmentTtlDays = clampInt(env.R2_ATTACHMENT_TTL_DAYS, 1, 365, 7);
  const rawTtlDays = clampInt(env.RAW_EVENT_TTL_DAYS, 1, 365, 30);
  const d1TtlDays = clampInt(env.D1_MESSAGE_TTL_DAYS, 1, 365, 30);

  const oldAttachments = await env.DB.prepare(`
    SELECT id, COALESCE(r2_object_key, local_path) AS object_key
    FROM download_jobs
    WHERE status = 'completed' AND completed_at < datetime('now', '-' || ? || ' days')
    LIMIT 200
  `).bind(attachmentTtlDays).all();
  for (const row of oldAttachments.results || []) {
    if (row.object_key) await env.RAW_BUCKET.delete(row.object_key);
    await env.DB.prepare(`UPDATE download_jobs SET status = 'purged', terminal_at = ?, updated_at = ? WHERE id = ?`).bind(new Date().toISOString(), new Date().toISOString(), row.id).run();
  }

  const oldRawObjects = await env.DB.prepare(`
    SELECT id, raw_object_key
    FROM raw_events
    WHERE raw_object_key IS NOT NULL AND received_at < datetime('now', '-' || ? || ' days')
    LIMIT 200
  `).bind(rawTtlDays).all();
  for (const row of oldRawObjects.results || []) {
    await env.RAW_BUCKET.delete(row.raw_object_key);
    await env.DB.prepare(`UPDATE raw_events SET raw_object_key = NULL WHERE id = ?`).bind(row.id).run();
  }

  await env.DB.prepare(`DELETE FROM download_jobs WHERE created_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
  await env.DB.prepare(`DELETE FROM chatroom_system_events WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
  await env.DB.prepare(`DELETE FROM chatroom_member_events WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
  await env.DB.prepare(`DELETE FROM chatroom_snapshots WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
	  await env.DB.prepare(`DELETE FROM messages WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
	  await env.DB.prepare(`DELETE FROM raw_events WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
	  await env.DB.prepare(`DELETE FROM webhook_checks WHERE received_at < datetime('now', '-' || ? || ' days')`).bind(d1TtlDays).run();
}

async function recordWebhookCheck(env, bodyText, sourceIp, userAgent) {
  await env.DB.prepare(`
    INSERT INTO webhook_checks (body_text, source_ip, user_agent)
    VALUES (?, ?, ?)
  `).bind(redactSensitiveBody(bodyText), sourceIp, userAgent).run();
}

function attachmentKind(normalized) {
  const type = String(normalized.msgType || "").toUpperCase();
  if (["IMAGE", "3"].includes(type)) return "image";
  if (["VOICE", "34"].includes(type)) return "voice";
  if (["VIDEO", "43", "62"].includes(type)) return "video";
  if (["EMOJI", "47"].includes(type)) return "emoji";
  if (isDownloadableFileMessage(normalized)) return "file";
  return null;
}

function isDownloadableFileMessage(normalized) {
  const type = String(normalized.msgType || "").toUpperCase();
  const content = normalized.rawContent || "";

  // GeWe v2 uses semantic msgType=FILE for downloadable files.
  if (type === "FILE") return hasDownloadableFileXml(content);

  // GeWe v1 MsgType=49 is a broad AppMsg container. Official docs define:
  // appmsg.type=5 link, 33/36 mini program, 57 quote, 74 file sending notification,
  // and only appmsg.type=6 as a completed downloadable file.
  if (type === "49") return normalized.appmsgType === 6 && hasDownloadableFileXml(content);

  return false;
}

function hasDownloadableFileXml(content) {
  if (!isLikelyXml(content)) return false;
  return /<appattach[\s>]/i.test(content) && xmlHasNonEmpty(content, "aeskey") && xmlHasNonEmpty(content, "cdnattachurl");
}

function xmlHasNonEmpty(content, name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`\\b${escaped}\\s*=\\s*["'][^"']+["']|<${escaped}>\\s*[^<\\s][\\s\\S]*?<\\/${escaped}>`, "i").test(content);
}

function downloadEndpoint(kind) {
  if (kind === "image") return "/gewe/v2/api/message/downloadImage";
  if (kind === "voice") return "/gewe/v2/api/message/downloadVoice";
  if (kind === "video") return "/gewe/v2/api/message/downloadVideo";
  if (kind === "file") return "/gewe/v2/api/message/downloadFile";
  if (kind === "emoji") return "/gewe/v2/api/message/downloadEmoji";
  return null;
}

function chooseConversationId({ fromUser, toUser, accountWxid, isOutgoing }) {
  if (isGroupConversation(fromUser)) return fromUser;
  if (isGroupConversation(toUser)) return toUser;
  if (isOutgoing) return toUser || fromUser || accountWxid || null;
  return fromUser || toUser || accountWxid || null;
}

function extractGroupSpeaker(fromUser, content) {
  if (!isGroupConversation(fromUser) || typeof content !== "string") return { senderWxid: null, content };
  const match = content.match(/^([^:\n]{3,128}):\n([\s\S]*)$/);
  if (!match) return { senderWxid: null, content };
  return { senderWxid: match[1], content: match[2] };
}

function parseAppMsgType(content) {
  if (typeof content !== "string") return null;
  const match = content.match(/<appmsg[\s\S]*?<type>(\d+)<\/type>/i) || content.match(/<type>(\d+)<\/type>/i);
  return match ? Number(match[1]) : null;
}

function isGroupConversation(value) {
  return typeof value === "string" && value.endsWith("@chatroom");
}

function isLikelyXml(value) {
  return typeof value === "string" && /^\s*</.test(value);
}

function ensureArray(value) {
  if (Array.isArray(value)) return value;
  if (value === null || value === undefined) return [];
  return [value];
}

function unwrap(value) {
  if (value && typeof value === "object") {
    if ("string" in value) return value.string;
    if ("buffer" in value) return value.buffer;
    if ("i64" in value) return value.i64;
  }
  return value;
}

function scalar(value) {
  const unwrapped = unwrap(value);
  if (unwrapped === null || unwrapped === undefined) return null;
  if (typeof unwrapped === "string") return unwrapped;
  if (typeof unwrapped === "number" || typeof unwrapped === "bigint" || typeof unwrapped === "boolean") return String(unwrapped);
  return JSON.stringify(unwrapped);
}

function intOrNull(value) {
  const number = Number(unwrap(value));
  return Number.isFinite(number) ? Math.trunc(number) : null;
}

function preview(value) {
  if (value === null || value === undefined) return null;
  const text = String(value);
  return text.length > MAX_PREVIEW_CHARS ? `${text.slice(0, MAX_PREVIEW_CHARS)}...` : text;
}

function awaitlessHashInput(value) {
  return JSON.stringify(value).slice(0, 2000);
}

function stableDedupeKey(schemaVersion, appid, identity) {
  return `${schemaVersion}:${appid}:${identity}`;
}

function toUpperSnake(value) {
  return String(value || "UNKNOWN")
    .replace(/([a-z0-9])([A-Z])/g, "$1_$2")
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toUpperCase() || "UNKNOWN";
}

function rawEventKey(schemaVersion, dedupeKey, receivedAt) {
  const date = receivedAt.slice(0, 10).replaceAll("-", "/");
  return `raw-events/${schemaVersion}/${date}/${encodeURIComponent(dedupeKey)}.json`;
}

function attachmentObjectKey(kind, jobKey, fileUrl, mimeType) {
  const date = new Date().toISOString().slice(0, 10).replaceAll("-", "/");
  const ext = extensionFromUrl(fileUrl) || extensionFromMime(mimeType) || ".bin";
  return `attachments/${kind}/${date}/${encodeURIComponent(jobKey)}${ext}`;
}

function extensionFromUrl(fileUrl) {
  try {
    const pathname = new URL(fileUrl).pathname;
    const match = pathname.match(/\.[A-Za-z0-9]{1,8}$/);
    return match ? match[0] : null;
  } catch (_error) {
    return null;
  }
}

function extensionFromMime(mimeType) {
  if (!mimeType) return null;
  const base = mimeType.split(";")[0].trim().toLowerCase();
  const map = {
    "image/jpeg": ".jpg",
    "image/png": ".png",
    "image/gif": ".gif",
    "image/webp": ".webp",
    "video/mp4": ".mp4",
    "audio/mpeg": ".mp3",
    "audio/amr": ".amr",
    "application/pdf": ".pdf"
  };
  return map[base] || null;
}

function normalizeDownloadedMime(assetType, mimeType, objectKeyOrUrl) {
  const base = mimeType ? mimeType.split(";")[0].trim().toLowerCase() : "";
  if (assetType === "voice" && /\.silk(?:$|\?)/i.test(objectKeyOrUrl || "")) {
    return "audio/silk";
  }
  if (assetType === "voice" && ["application/xml", "application/octst-stream", "application/octet-stream", ""].includes(base)) {
    return "audio/silk";
  }
  return mimeType || null;
}

async function sha256Hex(text) {
  const bytes = new TextEncoder().encode(text);
  const hash = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(hash)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function timingSafeEqual(a, b) {
  const left = new TextEncoder().encode(a);
  const right = new TextEncoder().encode(b);
  if (left.length !== right.length) return false;
  let diff = 0;
  for (let i = 0; i < left.length; i += 1) diff |= left[i] ^ right[i];
  return diff === 0;
}

function compactMetadata(metadata) {
  const output = {};
  for (const [key, value] of Object.entries(metadata)) {
    if (value !== null && value !== undefined && String(value)) output[key] = String(value).slice(0, 256);
  }
  return output;
}

function compactHeaders(headers) {
  const output = {};
  for (const [key, value] of Object.entries(headers)) {
    if (value !== null && value !== undefined && String(value)) output[key] = String(value);
  }
  return output;
}

function redactSensitiveBody(text) {
  if (!text) return text;
  try {
    return JSON.stringify(redactSensitiveValue(JSON.parse(text))).slice(0, 5000);
  } catch (_error) {
    return String(text)
      .replace(/(["']?(?:token|tokenId|tokenName|userId|gewe[_-]?token|x-gewe-token|authorization|api[_-]?key|secret|callbackUrl)["']?\\s*[:=]\\s*)(?:"[^"]*"|'[^']*'|[^\\s,}&]+)/gi, '$1"[redacted]"')
      .slice(0, 5000);
  }
}

function redactSensitiveValue(value) {
  if (Array.isArray(value)) return value.map(redactSensitiveValue);
  if (!value || typeof value !== "object") return value;

  const output = {};
  for (const [key, nested] of Object.entries(value)) {
    output[key] = isSensitiveKey(key) ? "[redacted]" : redactSensitiveValue(nested);
  }
  return output;
}

function isSensitiveKey(key) {
  return /^(token|tokenId|tokenName|userId|gewe[_-]?token|x-gewe-token|authorization|api[_-]?key|apikey|secret|callbackUrl)$/i.test(key);
}

function clampInt(value, min, max, fallback) {
  const number = Number(value);
  if (!Number.isFinite(number)) return fallback;
  return Math.max(min, Math.min(max, Math.trunc(number)));
}

function addSeconds(date, seconds) {
  return new Date(date.getTime() + (seconds * 1000));
}

function json(payload, status = 200) {
  return new Response(JSON.stringify(payload, null, 2), { status, headers: JSON_HEADERS });
}

function safeError(error) {
  if (!error) return "unknown error";
  return error.stack || error.message || String(error);
}
