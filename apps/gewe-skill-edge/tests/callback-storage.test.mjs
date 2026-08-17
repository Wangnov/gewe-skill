import assert from "node:assert/strict";
import test from "node:test";

import worker from "../src/index.ts";

class TestDatabase {
  constructor() {
    this.rawEventJson = null;
    this.messageContentXml = null;
    this.webhookCheckBody = null;
  }

  prepare(sql) {
    const database = this;
    return {
      values: [],
      bind(...values) {
        this.values = values;
        return this;
      },
      async run() {
        if (sql.includes("INSERT INTO raw_events")) database.rawEventJson = this.values[9];
        if (sql.includes("INSERT INTO messages")) database.messageContentXml = this.values[19];
        if (sql.includes("INSERT INTO webhook_checks")) database.webhookCheckBody = this.values[0];
        return { success: true };
      },
      async first() {
        if (sql.includes("FROM raw_events WHERE dedupe_key")) return { id: 1, duplicate_count: 0 };
        if (sql.includes("FROM messages WHERE message_key")) return { id: 1, duplicate_count: 0 };
        return null;
      },
      async all() {
        return { results: [] };
      }
    };
  }
}

function testEnvironment() {
  const database = new TestDatabase();
  const r2Writes = [];
  const waitUntilPromises = [];
  return {
    database,
    r2Writes,
    waitUntilPromises,
    env: {
      DB: database,
      RAW_BUCKET: {
        async put(key, body) {
          r2Writes.push({ key, body });
        }
      },
      ATTACHMENT_QUEUE: {
        async send() {}
      },
      CALLBACK_SECRET: "test-secret",
      ADMIN_API_KEY: "test-admin",
      EXPECTED_APP_ID: "test-app",
      EXPECTED_WXID: "wxid_test"
    },
    ctx: {
      waitUntil(promise) {
        waitUntilPromises.push(Promise.resolve(promise));
      }
    }
  };
}

test("stores and exports the complete callback body without redaction or truncation", async () => {
  const content = `<msg><appmsg><type>19</type><recorditem><![CDATA[<recordinfo><datalist><dataitem datatype="1"><datadesc>${"长回调内容".repeat(1800)}</datadesc></dataitem></datalist></recordinfo>]]></recorditem></appmsg></msg>`;
  const body = {
    TypeName: "AddMsg",
    Appid: "test-app",
    Wxid: "wxid_test",
    Data: {
      MsgId: { i64: "987654321" },
      NewMsgId: { i64: "987654321987654321" },
      MsgType: { i64: 49 },
      FromUserName: { string: "filehelper" },
      ToUserName: { string: "wxid_test" },
      Content: { string: content },
      CreateTime: { i64: 1786935600 },
      apiKey: "test-api-key-must-remain"
    }
  };
  const originalBody = JSON.stringify(body);
  assert.ok(originalBody.length > 5000);

  const fixture = testEnvironment();
  const response = await worker.fetch(
    new Request("https://example.test/callback/test-secret", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: originalBody
    }),
    fixture.env,
    fixture.ctx
  );
  await Promise.all(fixture.waitUntilPromises);

  assert.equal(response.status, 200);
  assert.equal(fixture.database.rawEventJson, originalBody);
  assert.equal(fixture.database.messageContentXml, content);
  assert.equal(fixture.r2Writes.length, 1);
  assert.equal(fixture.r2Writes[0].body, originalBody);
  assert.equal(JSON.parse(fixture.database.rawEventJson).Data.apiKey, body.Data.apiKey);
});

test("keeps the complete invalid callback body for webhook diagnostics", async () => {
  const invalidBody = "x".repeat(9000);
  const fixture = testEnvironment();
  const response = await worker.fetch(
    new Request("https://example.test/callback/test-secret", {
      method: "POST",
      headers: { "content-type": "text/plain" },
      body: invalidBody
    }),
    fixture.env,
    fixture.ctx
  );

  assert.equal(response.status, 200);
  assert.equal(fixture.database.webhookCheckBody, invalidBody);
});
