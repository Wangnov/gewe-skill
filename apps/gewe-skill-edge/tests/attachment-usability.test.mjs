import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { readFileSync } from "node:fs";
import worker, { detectAttachmentMime } from "../src/index.ts";

// 模拟 Workers 运行时：FixedLengthStream 产出长度已知的流，R2 拒收长度未知的流。
const knownLengthStreams = new WeakSet();
globalThis.FixedLengthStream ??= class FixedLengthStream extends TransformStream {
  constructor(length) {
    let seen = 0;
    super({
      transform(chunk, controller) {
        seen += chunk.byteLength;
        if (seen > length) throw new TypeError("FixedLengthStream received more bytes than declared");
        controller.enqueue(chunk);
      },
      flush() { if (seen !== length) throw new TypeError("FixedLengthStream received fewer bytes than declared"); }
    });
    knownLengthStreams.add(this.readable);
  }
};

// 使用真实 SQLite 执行 D1 SQL；所有身份、token 和附件均为测试数据。
function fixture() {
  const db = new DatabaseSync(":memory:");
  for (const migration of ["0001_initial.sql", "0002_download_job_queue_state.sql"]) {
    db.exec(readFileSync(new URL(`../migrations/${migration}`, import.meta.url), "utf8"));
  }
  const sent = [];
  const objects = new Map();
  const pending = [];
  const env = {
    ADMIN_API_KEY: "test-admin", CALLBACK_SECRET: "test-callback", GEWE_TOKEN: "test-upstream",
    GEWE_API_BASE: "https://api.example.test", IMAGE_DOWNLOAD_TYPE: "2",
    DB: {
      prepare(sql) {
        let values = [];
        return {
          bind(...args) { values = args; return this; },
          async first() { return db.prepare(sql).get(...values) || null; },
          async all() { return { results: db.prepare(sql).all(...values) }; },
          async run() { const result = db.prepare(sql).run(...values); return { success: true, meta: { changes: Number(result.changes) } }; }
        };
      }
    },
    ATTACHMENT_QUEUE: { async send(value) { sent.push(value); } },
    RAW_BUCKET: { async put(key, body, options) {
      if (body instanceof ReadableStream && !knownLengthStreams.has(body)) {
        throw new TypeError("Provided readable stream must have a known length (request/response body or readable half of FixedLengthStream)");
      }
      objects.set(key, { bytes: new Uint8Array(await new Response(body).arrayBuffer()), options });
    } }
  };
  const ctx = { waitUntil(promise) { pending.push(Promise.resolve(promise)); } };
  return { db, env, ctx, sent, objects, pending };
}

async function message(f, id) {
  const response = await worker.fetch(new Request("https://example.test/callback/test-callback", {
    method: "POST", body: JSON.stringify({ appid: "test-app", wxid: "wxid_test_account", newMsgId: id,
      msgType: "3", fromUser: "wxid_test_friend", toUser: "wxid_test_account", content: '<msg><img aeskey="fake" /></msg>' })
  }), f.env, f.ctx);
  assert.equal(response.status, 200);
  await Promise.all(f.pending);
  return f.db.prepare("SELECT id, message_key FROM messages WHERE new_msg_id = ?").get(id);
}

async function admin(f, path, method = "GET") {
  return worker.fetch(new Request(`https://example.test${path}`, { method, headers: { authorization: "Bearer test-admin" } }), f.env, f.ctx);
}

test("定向补拉只为指定消息补建任务，未知消息不会触发全局操作", async () => {
  const f = fixture();
  try {
    const first = await message(f, "101");
    const second = await message(f, "102");
    f.db.prepare("DELETE FROM download_jobs WHERE message_id = ?").run(first.id);
    f.sent.length = 0;
    const response = await admin(f, `/admin/repair-attachment?message_key=${encodeURIComponent(first.message_key)}`, "POST");
    assert.equal(response.status, 200);
    assert.equal((await response.json()).queued_count, 1);
    assert.deepEqual(f.sent, [{ jobKey: `${first.message_key}:image` }]);
    const jobs = await (await admin(f, `/admin/download-jobs?message_key=${encodeURIComponent(first.message_key)}`)).json();
    assert.equal(jobs.jobs.length, 1);
    assert.equal(jobs.jobs[0].message_key, first.message_key);
    assert.equal(f.db.prepare("SELECT status FROM download_jobs WHERE message_id = ?").get(second.id).status, "pending");
    assert.equal((await admin(f, "/admin/repair-attachment?message_key=v2:test-app:missing", "POST")).status, 404);
    assert.equal((await admin(f, "/admin/repair-attachment", "POST")).status, 400);
    assert.equal(f.sent.length, 1);
  } finally { f.db.close(); }
});

test("定向重排只重置该消息失败任务，保留正在使用的租约与已完成任务", async () => {
  const f = fixture();
  try {
    const first = await message(f, "201");
    const second = await message(f, "202");
    f.db.prepare("UPDATE download_jobs SET status='failed', attempts=6, last_error='test-error'").run();
    f.sent.length = 0;
    const path = `/admin/repair-attachment?message_key=${encodeURIComponent(first.message_key)}`;
    await admin(f, path, "POST");
    const firstJob = f.db.prepare("SELECT * FROM download_jobs WHERE message_id = ?").get(first.id);
    assert.equal(firstJob.status, "pending"); assert.equal(firstJob.attempts, 0); assert.equal(firstJob.last_error, null);
    assert.equal(f.db.prepare("SELECT status FROM download_jobs WHERE message_id = ?").get(second.id).status, "failed");
    assert.equal(f.sent.length, 1);
    f.db.prepare("UPDATE download_jobs SET status='processing', locked_until='2999-01-01T00:00:00Z' WHERE message_id = ?").run(first.id);
    assert.equal((await (await admin(f, path, "POST")).json()).queued_count, 0);
    f.db.prepare("UPDATE download_jobs SET status='completed', r2_object_key='test-object' WHERE message_id = ?").run(first.id);
    assert.equal((await (await admin(f, path, "POST")).json()).queued_count, 0);
    assert.equal(f.sent.length, 1);
  } finally { f.db.close(); }
});

test("附件清单按消息过滤，不能返回另一条消息的附件", async () => {
  const f = fixture();
  try {
    const first = await message(f, "301"); await message(f, "302");
    f.db.prepare("UPDATE download_jobs SET status='completed', r2_object_key='test-object'").run();
    const response = await admin(f, `/admin/attachments?message_key=${encodeURIComponent(first.message_key)}`);
    const data = await response.json();
    assert.equal(data.attachments.length, 1);
    assert.equal(data.attachments[0].message_key, first.message_key);
  } finally { f.db.close(); }
});

test("edge 文件头识别覆盖常见媒体和文件格式", () => {
  const encode = (s) => new TextEncoder().encode(s);
  for (const [bytes, mime] of [
    [new Uint8Array([255,216,255]), "image/jpeg"], [new Uint8Array([137,80,78,71,13,10,26,10]), "image/png"],
    [encode("GIF87a"), "image/gif"], [encode("GIF89a"), "image/gif"], [encode("RIFF0000WEBP"), "image/webp"],
    [encode("0000ftypmp42"), "video/mp4"], [encode("#!SILK_V3"), "audio/silk"],
    [new Uint8Array([2, ...encode("#!SILK_V3")]), "audio/silk"], [encode("#!AMR\n"), "audio/amr"],
    [encode("#!AMR-WB\n"), "audio/amr-wb"], [encode("%PDF-1.7"), "application/pdf"], [new Uint8Array([80,75,3,4]), "application/zip"]
  ]) assert.equal(detectAttachmentMime(bytes), mime);
  assert.equal(detectAttachmentMime(encode("RIFF")), null);
  assert.equal(detectAttachmentMime(encode("0000ftypheic")), null);
  assert.equal(detectAttachmentMime(encode("0000ftypavif")), null);
  assert.equal(detectAttachmentMime(new Uint8Array()), null);
});

test("queue 按魔数覆盖错误 MIME，分块读取后存入 R2 的字节完整", async (t) => {
  const f = fixture();
  try {
    const first = await message(f, "401");
    const bytes = new Uint8Array(100000).fill(42); bytes.set([255,216,255,224]);
    t.mock.method(globalThis, "fetch", async (url, options) => {
      if (String(url).startsWith("https://api.example.test")) return Response.json({ ret: 200, data: { fileUrl: "https://file.example.test/image" } });
      if (options?.method === "HEAD") return new Response(null, { headers: { "content-length": String(bytes.length), "content-type": "application/octst-stream" } });
      let offset = 0;
      return new Response(new ReadableStream({ pull(controller) {
        if (offset >= bytes.length) { controller.close(); return; }
        const end = Math.min(offset + (offset < 64 ? 2 : 8192), bytes.length);
        controller.enqueue(bytes.slice(offset, end)); offset = end;
      } }), { headers: { "content-length": String(bytes.length), "content-type": "image/png" } });
    });
    await worker.queue({ messages: [{ body: { jobKey: `${first.message_key}:image` }, ack() {}, retry() { assert.fail("任务不应失败"); } }] }, f.env);
    const job = f.db.prepare("SELECT * FROM download_jobs WHERE message_id = ?").get(first.id);
    assert.equal(job.status, "completed"); assert.equal(job.mime_type, "image/jpeg");
    assert.deepEqual(f.objects.get(job.r2_object_key).bytes, bytes);
    assert.equal(f.objects.get(job.r2_object_key).options.httpMetadata.contentType, "image/jpeg");
  } finally { f.db.close(); }
});

test("当天下载次数超限：排到北京时间零点后重试，不扣重试次数", async (t) => {
  const f = fixture();
  try {
    t.mock.timers.enable({ apis: ["Date"], now: Date.parse("2026-10-23T11:00:00Z") }); // 北京时间 19:00
    const first = await message(f, "501");
    t.mock.method(globalThis, "fetch", async () => Response.json({ ret: 500, msg: "com.gewe.common.exception.base.BaseException: test-app 当天上传下载次数超限！" }));
    await worker.queue({ messages: [{ body: { jobKey: `${first.message_key}:image` }, ack() {}, retry() {} }] }, f.env);
    const job = f.db.prepare("SELECT * FROM download_jobs WHERE message_id = ?").get(first.id);
    assert.equal(job.status, "retry_scheduled");
    assert.equal(job.attempts, 0);
    assert.ok(job.next_attempt_at >= "2026-10-23T16:05:00.000Z" && job.next_attempt_at <= "2026-10-23T16:30:00.000Z", job.next_attempt_at);
    await worker.queue({ messages: [{ body: { jobKey: `${first.message_key}:image` }, ack() {}, retry() {} }] }, f.env);
    assert.equal(f.db.prepare("SELECT attempts FROM download_jobs WHERE message_id = ?").get(first.id).attempts, 0);
  } finally { f.db.close(); }
});

test("暂停的积压只在北京时间凌晨分批放行，最老的先放，表情不放，当天超限就停", async (t) => {
  const f = fixture();
  try {
    f.env.PAUSED_RELEASE_PER_RUN = "2";
    const rows = [];
    for (const [id, created] of [["601", "2026-10-15T01:00:00Z"], ["602", "2026-10-16T01:00:00Z"], ["603", "2026-10-17T01:00:00Z"], ["604", "2026-10-14T20:00:00Z"]]) {
      const m = await message(f, id);
      f.db.prepare("UPDATE download_jobs SET status = 'paused', attempts = 6, created_at = ? WHERE message_id = ?").run(created, m.id);
      rows.push(m);
    }
    f.db.prepare("UPDATE download_jobs SET asset_type = 'emoji' WHERE message_id = ?").run(rows[3].id);
    const status = (m) => f.db.prepare("SELECT status, attempts FROM download_jobs WHERE message_id = ?").get(m.id);
    const tick = async (iso) => {
      t.mock.timers.reset(); t.mock.timers.enable({ apis: ["Date"], now: Date.parse(iso) });
      await worker.scheduled({ cron: "*/10 * * * *" }, f.env, f.ctx); await Promise.all(f.pending);
    };

    await tick("2026-10-23T08:00:00Z"); // 北京时间 16:00，不放行
    assert.deepEqual(rows.map((m) => status(m).status), ["paused", "paused", "paused", "paused"]);

    await tick("2026-10-23T16:20:00Z"); // 北京时间 00:20，放行最老的两个非表情任务
    assert.deepEqual(rows.map((m) => status(m).status), ["pending", "pending", "paused", "paused"]);
    assert.equal(status(rows[0]).attempts, 0);

    const deferred = await message(f, "605");
    f.db.prepare("UPDATE download_jobs SET status = 'retry_scheduled', next_attempt_at = '2026-10-24T16:10:00Z' WHERE message_id = ?").run(deferred.id);
    await tick("2026-10-23T16:30:00Z"); // 当天已超限，停止放行
    assert.equal(status(rows[2]).status, "paused");
    assert.equal(status(rows[3]).status, "paused");
  } finally { f.db.close(); }
});
