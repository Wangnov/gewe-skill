#!/usr/bin/env python3
"""Local end-to-end tests. Only temporary SQLite/files, fake identities and loopback HTTP/SSH."""
import hashlib
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "target/debug/gewe-skill"
MEMORY = ROOT / "target/debug/gewe-skill-memory"
WRAPPER = ROOT / "scripts/gewe.sh"
JPEG = b"\xff\xd8\xff\xe0test-jpeg-attachment"
PNG = b"\x89PNG\r\n\x1a\ntest-png-attachment"


def executable(path, body):
    path.write_text(body)
    path.chmod(0o755)


def fake_ssh(directory):
    executable(directory / "ssh", """#!/usr/bin/env python3
import os, subprocess, sys
if os.environ.get('TEST_SSH_FAIL'):
    sys.stderr.write('raw ssh failure must not be the only explanation\\n')
    sys.exit(255)
sys.exit(subprocess.call(['/bin/sh', '-c', sys.argv[-1]]))
""")
    executable(directory / "sudo", """#!/usr/bin/env bash
if [[ "${1:-}" == -n ]]; then shift; fi
exec "$@"
""")


class WrapperArguments(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gewe-wrapper-test-")
        self.directory = Path(self.temp.name)
        fake_ssh(self.directory)
        echo_cli = self.directory / "echo-cli"
        executable(echo_cli, "#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:], ensure_ascii=False))\n")
        config = self.directory / "test.env"
        config.write_text("# Test-only empty remote configuration\n")
        self.env = dict(os.environ, PATH=f"{self.directory}:{os.environ['PATH']}",
                        GEWE_REMOTE_ENV=str(config), GEWE_REMOTE_CLI=str(echo_cli), GEWE_SSH_TARGET="test-host", GEWE_SSH_KEY="/dev/null")

    def tearDown(self):
        self.temp.cleanup()

    def run_wrapper(self, args, **kwargs):
        return subprocess.run([str(WRAPPER), *args], env=self.env, capture_output=True, **kwargs)

    def test_safe_unicode_quotes_spaces_newline_and_shell_syntax(self):
        marker = self.directory / "must-not-exist"
        value = f"中文 空格 ' 单引号 \" 双引号\n$(touch {marker}); `touch {marker}`"
        args = ["query", "messages", "--conversation", value, "--q", "a'b\"c\\d"]
        result = self.run_wrapper(args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["--json", *args])
        self.assertFalse(marker.exists())

    def test_text_and_explicit_json_are_not_duplicated(self):
        args = ["query", "messages", "--format", "text"]
        self.assertEqual(json.loads(self.run_wrapper(args).stdout), args)
        args = ["--json", "doctor"]
        self.assertEqual(json.loads(self.run_wrapper(args).stdout), args)

    def test_tailscale_error_is_readable(self):
        self.env["TEST_SSH_FAIL"] = "1"
        result = self.run_wrapper(["doctor"])
        self.assertEqual(result.returncode, 255)
        self.assertIn("检查 Tailscale", result.stderr.decode())
        self.assertNotIn("raw ssh failure", result.stderr.decode())

    def test_old_cli_cannot_write_dash_as_a_remote_filename(self):
        destination = self.directory / "existing.jpg"
        destination.write_bytes(b"original")
        result = self.run_wrapper(["attachments", "download", "--sha256", "fake-sha", "-o", str(destination)])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("远端 CLI 尚不支持", result.stderr.decode())
        self.assertEqual(destination.read_bytes(), b"original")


class LocalMemory(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="gewe-usability-test-")
        cls.directory = Path(cls.temp.name)
        cls.db = cls.directory / "memory.sqlite"
        cls.attachments = cls.directory / "attachments"
        cls.attachments.mkdir()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            cls.port = sock.getsockname()[1]
        cls.url = f"http://127.0.0.1:{cls.port}"
        cls.env = {"PATH": os.environ["PATH"], "HOME": os.environ["HOME"], "TZ": "Asia/Shanghai",
                   "GEWE_SKILL_DATABASE_URL": f"sqlite:{cls.db}?mode=rwc", "GEWE_SKILL_LISTEN": f"127.0.0.1:{cls.port}",
                   "GEWE_SKILL_ATTACHMENT_DIR": str(cls.attachments), "GEWE_SKILL_BASE_URL": cls.url,
                   "GEWE_SKILL_READ_TOKEN": "test-read", "GEWE_SKILL_WRITE_TOKEN": "test-write",
                   "GEWE_SKILL_AUTO_TRANSCRIBE_VOICE": "false", "GEWE_SKILL_ASR_BACKGROUND_ENABLED": "false"}
        cls.logs = tempfile.TemporaryFile()
        cls.process = subprocess.Popen([str(MEMORY)], env=cls.env, stdout=cls.logs, stderr=cls.logs)
        try:
            for _ in range(100):
                try:
                    cls.api("/healthz")
                    break
                except (urllib.error.URLError, ConnectionError):
                    if cls.process.poll() is not None:
                        raise RuntimeError("temporary memory service exited")
                    time.sleep(0.05)
            else:
                raise RuntimeError("temporary memory service did not become healthy")
            cls.seed()
        except Exception:
            cls.tearDownClass()
            raise

    @classmethod
    def tearDownClass(cls):
        cls.process.terminate()
        try:
            cls.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            cls.process.kill()
            cls.process.wait()
        cls.logs.close()
        cls.temp.cleanup()

    @classmethod
    def api(cls, path, body=None, headers=False):
        token = "test-write" if path.startswith("/write/") else "test-read"
        request = urllib.request.Request(cls.url + path, data=None if body is None else json.dumps(body).encode(),
                                         headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as response:
            if headers:
                return response.read(), dict(response.headers)
            return json.load(response)

    @classmethod
    def message(cls, number, kind, content, hour, *, outgoing=False, group=False):
        body = {"appid": "test-app", "wxid": "wxid_test_account", "newMsgId": str(number), "msgType": kind,
                "fromUser": "wxid_test_account" if outgoing else "wxid_test_friend",
                "toUser": "wxid_test_friend" if outgoing else "wxid_test_account", "isSelf": outgoing, "content": content}
        if group:
            body["fromGroup"] = "test_group_a@chatroom"
        return cls.api("/write/raw-events", {"received_at": f"2026-10-12T{hour}:00Z", "body": body})["message_key"]

    @classmethod
    def attachment(cls, key, content, job_id, *, kind="image", variant=None):
        sha = hashlib.sha256(content).hexdigest()
        object_key = f"sha256/{sha[:2]}/{sha}"
        path = cls.attachments / object_key
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        record = {"id": None, "edge_job_id": job_id, "job_key": f"test-job-{job_id}", "message_key": key,
                  "raw_event_dedupe_key": key, "appid": "test-app", "account_wxid": "wxid_test_account",
                  "kind": kind, "variant": variant, "source_url": None, "object_key": object_key, "sha256": sha,
                  "size_bytes": len(content), "mime_type": "application/octst-stream", "created_at": "2026-10-12T19:00:00Z"}
        cls.api("/write/attachments", record)
        return sha

    @classmethod
    def seed(cls):
        now = "2026-10-12T16:00:00Z"
        with sqlite3.connect(cls.db) as db:
            for wxid, name in [("wxid_test_friend", "测试好友"), ("wxid_test_nodm", "无私聊好友"), ("wxid_test_other", "测试群")]:
                db.execute("INSERT INTO identity_contacts(wxid,nickname,remark,last_seen_at,updated_at) VALUES(?,?,?,?,?)", (wxid, "普通昵称", name, now, now))
                for source in ["contact_remark", "gewe_contact", "observed_alias"]:
                    db.execute("INSERT INTO identity_aliases(alias_key,alias,entity_type,entity_id,scope_key,source,is_current,confidence,first_seen_at,last_seen_at) VALUES(?,?,?,?,?,?,1,1,?,?)",
                               (name, name, "contact", wxid, "", source, now, now))
            for room, name in [("test_group_a@chatroom", "测试群"), ("test_group_b@chatroom", "另一个群")]:
                db.execute("INSERT INTO identity_chatrooms(chatroom_id,display_name,last_seen_at,updated_at) VALUES(?,?,?,?)", (room, name, now, now))
                db.execute("INSERT INTO identity_aliases(alias_key,alias,entity_type,entity_id,scope_key,source,is_current,confidence,first_seen_at,last_seen_at) VALUES(?,?,?,?,?,?,1,1,?,?)",
                           (name, name, "chatroom", room, "", "group_name", now, now))
                for wxid, name in [("wxid_test_friend", "测试好友"), ("wxid_test_nodm", "无私聊好友")]:
                    db.execute("INSERT INTO identity_chatroom_members(chatroom_id,member_wxid,display_name,nickname,is_current,first_seen_at,last_seen_at) VALUES(?,?,?,?,1,?,?)",
                               (room, wxid, "群名片", name, now, now))
                    db.execute("INSERT INTO identity_aliases(alias_key,alias,entity_type,entity_id,scope_key,source,is_current,confidence,first_seen_at,last_seen_at) VALUES(?,?,?,?,?,?,1,1,?,?)",
                               (name, name, "chatroom_member", wxid, room, "member_nickname", now, now))
        cls.message(1, 1, "范围之前", "15:00")
        cls.message(2, 1, "边界消息", "16:00")
        cls.message(3, 1, "重复时间一", "17:00")
        cls.message(4, 1, "重复时间二", "17:00")
        cls.self_key = cls.message(5, 1, "自发消息", "18:00", outgoing=True)
        cls.image_key = cls.message(6, 3, '<msg><img aeskey="fake" /></msg>', "19:00")
        cls.missing_key = cls.message(7, 3, '<msg><img aeskey="fake" /></msg>', "19:30")
        cls.quote_key = cls.message(8, 49, '<msg><appmsg><title>引用的正文</title><type>57</type><refermsg><content><![CDATA[被引用的内容]]></content></refermsg></appmsg></msg>', "20:00")
        cls.voice_key = cls.message(9, 34, '<msg><voicemsg /></msg>', "21:00")
        cls.message(10, 10000, '<sysmsg><template><![CDATA[$username$邀请$names$加入了群聊]]></template><link name="username"><member><username>wxid_test_friend</username><nickname>测试好友</nickname></member></link><link name="names"><member><username>wxid_test_joiner</username><nickname>测试成员</nickname></member></link></sysmsg>', "22:00", group=True)
        cls.message(11, 1, "群内正文", "22:30", group=True)
        cls.jpeg_sha = cls.attachment(cls.image_key, JPEG, 1)
        cls.attachment(cls.voice_key, b"\x02#!SILK_V3test-voice", 2, kind="voice")
        cls.multi_key = cls.message(12, 3, '<msg><img /></msg>', "23:00", group=True)
        cls.attachment(cls.multi_key, JPEG, 3, variant="original")
        cls.attachment(cls.multi_key, PNG, 4, variant="thumbnail")
        with sqlite3.connect(cls.db) as db:
            db.execute("INSERT INTO voice_transcripts(message_key,provider,text,status,created_at,updated_at) VALUES(?,?,?,'completed',?,?)",
                       (cls.voice_key, "test-asr", "测试语音转写", now, now))
        cls.fake_bin = cls.directory / "fake-bin"
        cls.fake_bin.mkdir()
        fake_ssh(cls.fake_bin)
        cls.config = cls.directory / "test-remote.env"
        # Every value is synthetic. Never source or copy production configuration in tests.
        cls.config.write_text(f"GEWE_SKILL_BASE_URL='{cls.url}'\nGEWE_SKILL_READ_TOKEN='test-read'\nGEWE_SKILL_WRITE_TOKEN='test-write'\n")

    def cli(self, *args, check=True):
        result = subprocess.run([str(CLI), *args], env=self.env, capture_output=True, timeout=15)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result

    def wrapper(self, *args, env=None, check=True):
        wrapper_env = dict(self.env, PATH=f"{self.fake_bin}:{self.env['PATH']}", GEWE_REMOTE_ENV=str(self.config),
                           GEWE_REMOTE_CLI=str(CLI), GEWE_SSH_TARGET="test-host", GEWE_SSH_KEY="/dev/null")
        wrapper_env.update(env or {})
        result = subprocess.run([str(WRAPPER), *args], env=wrapper_env, capture_output=True, timeout=15)
        if check:
            self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result

    def test_friend_resolves_to_dm_and_candidates_merge_sources_and_rooms(self):
        data = json.loads(self.cli("query", "messages", "--conversation", "测试好友").stdout)
        self.assertEqual(data["resolved"]["conversation"]["id"], "wxid_test_friend")
        candidates = data["resolved"]["conversation"]["candidates"]
        friend = next(item for item in candidates if item["entity_id"] == "wxid_test_friend")
        self.assertEqual(len([item for item in candidates if item["entity_id"] == "wxid_test_friend"]), 1)
        self.assertGreaterEqual(len(friend["sources"]), 4)
        self.assertEqual(len(friend["chatroom_ids"]), 2)
        self.assertEqual(data["messages"]["pagination"]["selection"], "latest")

    def test_group_name_and_dm_unresolved(self):
        data = json.loads(self.cli("query", "messages", "--conversation", "测试群").stdout)
        self.assertEqual(data["resolved"]["conversation"]["id"], "test_group_a@chatroom")
        data = json.loads(self.cli("query", "messages", "--conversation", "无私聊好友", "--dm").stdout)
        self.assertFalse(data["executed"])
        self.assertEqual(data["error"], "dm_conversation_unresolved")
        self.assertIn("未找到", data["hint"])
        text = self.cli("query", "messages", "--conversation", "无私聊好友", "--dm", "--format", "text").stdout.decode()
        self.assertIn("未找到该联系人的私聊会话", text)
        data = json.loads(self.cli("query", "messages", "--dm").stdout)
        self.assertTrue(all(not item["is_group"] for item in data["messages"]["items"]))

    def test_after_keeps_latest_and_ascending_means_earliest(self):
        base = ["messages", "list", "--conversation-id", "wxid_test_friend", "--after", "2026-10-13T00:00:00+08:00", "--limit", "1"]
        newest = json.loads(self.cli(*base).stdout)
        self.assertEqual(newest["items"][0]["message_key"], self.voice_key)
        self.assertEqual(newest["pagination"]["selection"], "latest")
        oldest = json.loads(self.cli(*base, "--order", "asc").stdout)
        self.assertEqual(oldest["items"][0]["content_text"], "边界消息")
        self.assertFalse(oldest["pagination"]["has_earlier"])
        self.assertTrue(oldest["pagination"]["has_later"])

    def test_equal_timestamps_are_not_skipped_and_search_has_text(self):
        base = ["messages", "search", "--q", "重复时间", "--limit", "1"]
        first = json.loads(self.cli(*base).stdout)
        second = json.loads(self.cli(*base, "--cursor", first["next_cursor"]).stdout)
        self.assertNotEqual(first["items"][0]["message_key"], second["items"][0]["message_key"])
        self.assertIsNone(second["next_cursor"])
        text = self.cli(*base, "--format", "text").stdout.decode()
        self.assertIn("10-13 01:00 测试好友: 重复时间", text)
        self.assertIn("--cursor", text)

    def test_text_names_markers_summary_timezone_and_context(self):
        text = self.cli("query", "messages", "--conversation", "测试好友", "--after", "2026-10-13T00:00:00+08:00", "--order", "asc", "--format", "text").stdout.decode()
        self.assertIn("10-13 00:00 测试好友: 边界消息", text)
        self.assertIn("我: 自发消息", text)
        self.assertIn(f"[图片 {self.jpeg_sha[:8]}]", text)
        self.assertIn("[图片 附件缺失]", text)
        self.assertIn("[语音 测试语音转写]", text)
        self.assertIn("[引用 被引用的内容] 引用的正文", text)
        self.assertIn("范围内最早 N 条", text)
        self.assertIn("附件缺失 1 条", text)
        self.assertEqual(text.count("\n"), 9)
        utc = self.cli("messages", "list", "--q", "边界消息", "--format", "text", "--timezone", "UTC", check=False)
        # q belongs to search; the parser must reject a misplaced query option.
        self.assertNotEqual(utc.returncode, 0)
        utc = self.cli("messages", "search", "--q", "边界消息", "--format", "text", "--timezone", "UTC").stdout.decode()
        self.assertIn("10-12 16:00", utc)
        context = self.cli("messages", "context", "--message-key", self.quote_key, "--before", "1", "--after", "1", "--format", "text").stdout.decode()
        self.assertIn("消息上下文", context)
        self.assertIn("共 3 条", context)
        group = self.cli("query", "messages", "--conversation", "测试群", "--format", "text").stdout.decode()
        self.assertIn("测试好友邀请测试成员加入了群聊", group)
        self.assertNotIn("<sysmsg", group)
        self.assertIn("测试好友: 群内正文", group)

    def test_compact_json_keeps_normal_default_fields(self):
        base = ["query", "messages", "--conversation", "测试好友"]
        normal = json.loads(self.cli(*base).stdout)
        compact = json.loads(self.cli(*base, "--compact").stdout)
        self.assertIn("raw_content", normal["messages"]["items"][0])
        self.assertIn("msg_source", normal["messages"]["items"][0])
        self.assertNotIn("raw_content", compact["messages"]["items"][0])
        self.assertEqual(normal["messages"]["items"][0]["message_key"], compact["messages"]["items"][0]["message_key"])

    def test_download_by_message_stdout_local_path_multiple_selection_and_missing_hint(self):
        result = self.cli("attachments", "download", "--message-key", self.image_key, "-o", "-")
        self.assertEqual(result.stdout, JPEG)
        destination = self.directory / "中文 引号' 附件.jpg"
        self.wrapper("attachments", "download", "--message-key", self.image_key, "-o", str(destination))
        self.assertEqual(destination.read_bytes(), JPEG)
        destination.write_bytes(b"original-file")
        failed = self.wrapper("attachments", "download", "--message-key", self.missing_key, "-o", str(destination), check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(destination.read_bytes(), b"original-file")
        self.assertIn("attachment-repair", failed.stderr.decode())
        multiple = self.cli("attachments", "download", "--message-key", self.multi_key, check=False)
        self.assertNotEqual(multiple.returncode, 0)
        self.assertIn("--index N", multiple.stderr.decode())
        records = json.loads(self.cli("attachments", "list", "--message-key", self.multi_key).stdout)["items"]
        selected = self.cli("attachments", "download", "--message-key", self.multi_key, "--index", "1")
        self.assertEqual(hashlib.sha256(selected.stdout).hexdigest(), records[0]["sha256"])

    def test_export_is_written_to_local_output_and_stdout_contains_only_document(self):
        destination = self.directory / "朋友圈 导出.md"
        result = self.wrapper("sns", "export", "--no-refresh", "-o", str(destination))
        self.assertEqual(result.stdout, b"")
        self.assertTrue(destination.read_text().startswith("# "))
        result = self.cli("sns", "export", "--no-refresh", "--output", "-")
        self.assertTrue(result.stdout.startswith(b"# "))
        self.assertNotIn(b'"post_count"', result.stdout)

    def test_mime_insert_read_download_and_dry_run_then_apply(self):
        with sqlite3.connect(self.db) as db:
            record = json.loads(db.execute("SELECT attachment_json FROM attachments WHERE edge_job_id=1").fetchone()[0])
            self.assertEqual(record["mime_type"], "image/jpeg")
            record["mime_type"] = "application/octst-stream"
            db.execute("UPDATE attachments SET mime_type=?,attachment_json=? WHERE edge_job_id=1", (record["mime_type"], json.dumps(record)))
        read = self.api("/api/attachments/by-message-keys", {"message_keys": [self.image_key]})
        self.assertEqual(read["items"][0]["mime_type"], "image/jpeg")
        body, headers = self.api(f"/api/attachments/{self.jpeg_sha}/download", headers=True)
        self.assertEqual(body, JPEG)
        self.assertEqual(headers["content-type"], "image/jpeg")
        report = json.loads(self.cli("maintenance", "attachment-mime-backfill", "--dry-run").stdout)
        self.assertEqual(report["changed"], 1)
        with sqlite3.connect(self.db) as db:
            self.assertEqual(db.execute("SELECT mime_type FROM attachments WHERE edge_job_id=1").fetchone()[0], "application/octst-stream")
        report = json.loads(self.cli("maintenance", "attachment-mime-backfill", "--apply").stdout)
        self.assertEqual(report["changed"], 1)
        self.assertEqual(json.loads(self.cli("maintenance", "attachment-mime-backfill").stdout)["changed"], 0)

    def test_other_media_markers_never_show_xml(self):
        self.message(121, 49, '<msg><appmsg><title>方案.pdf</title><type>6</type><appattach><totallen>10</totallen></appattach></appmsg></msg>', "23:40", group=True)
        self.message(122, 49, '<msg><appmsg><title>示例链接</title><type>5</type><url>https://example.test/</url></appmsg></msg>', "23:41", group=True)
        self.message(123, 47, '<msg><emoji md5="test" /></msg>', "23:42", group=True)
        text = self.cli("query", "messages", "--conversation-id", "test_group_a@chatroom", "--format", "text").stdout.decode()
        self.assertIn("[文件 方案.pdf]", text)
        self.assertIn("[链接 示例链接]", text)
        self.assertIn("[表情]", text)
        self.assertNotIn("<appmsg", text)
        self.assertNotIn("<emoji", text)

    def test_targeted_repair_only_touches_the_message_and_never_global_cursor(self):
        repair_key = self.message(100, 3, '<msg><img /></msg>', "23:30", group=True)
        content = b"\xff\xd8\xfftargeted-image"
        requests = []
        state = {"pending": False}
        class Edge(BaseHTTPRequestHandler):
            def do_POST(self):
                requests.append(self.path)
                self.send_json({"ok": True, "queued_count": 1})
            def do_GET(self):
                requests.append(self.path)
                path = urllib.parse.urlparse(self.path)
                if path.path == "/admin/attachment":
                    self.send_response(200); self.end_headers(); self.wfile.write(content)
                elif path.path == "/admin/attachments":
                    attachments = [{"job_id": 100, "job_key": "test-target-job", "message_key": repair_key,
                        "appid": "test-app", "asset_type": "image", "size_bytes": len(content), "mime_type": "application/octst-stream", "download_path": "/admin/attachment?job_id=100"}]
                    self.send_json({"attachments": [] if state["pending"] else attachments, "next_after_job_id": 100})
                else:
                    self.send_json({"jobs": [{"job_id": 100, "message_key": repair_key, "status": "pending" if state["pending"] else "completed"}]})
            def send_json(self, value):
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers(); self.wfile.write(json.dumps(value).encode())
            def log_message(self, *_):
                pass
        server = ThreadingHTTPServer(("127.0.0.1", 0), Edge)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        cursor = self.directory / "global.cursor"
        cursor.write_text("123\n")
        try:
            report = json.loads(self.cli("sync", "attachment-repair", "--message-key", repair_key, "--edge-url", f"http://127.0.0.1:{server.server_port}", "--admin-token", "test-admin",
                "--attachment-dir", str(self.attachments), "--cursor-file", str(cursor), "--wait-seconds", "3").stdout)
            self.assertTrue(report["ok"])
            self.assertEqual(cursor.read_text(), "123\n")
            self.assertEqual(self.cli("attachments", "download", "--message-key", repair_key).stdout, content)
            self.assertTrue(all("message_key=" in path for path in requests if not path.startswith("/admin/attachment?")))
            self.assertFalse(any("backfill" in path or "requeue" in path or "stats" in path for path in requests))
            state["pending"] = True
            timeout = json.loads(self.cli("sync", "attachment-repair", "--message-key", repair_key, "--edge-url", f"http://127.0.0.1:{server.server_port}", "--admin-token", "test-admin",
                "--attachment-dir", str(self.attachments), "--cursor-file", str(cursor), "--wait-seconds", "1").stdout)
            self.assertEqual(timeout["status"], "timeout")
            self.assertFalse(timeout["ok"])
            self.assertEqual(cursor.read_text(), "123\n")
        finally:
            server.shutdown(); server.server_close(); thread.join()


if __name__ == "__main__":
    if not CLI.is_file() or not MEMORY.is_file():
        raise SystemExit("先运行 cargo build --locked -p gewe-skill-cli -p gewe-skill-memory --bins")
    unittest.main(verbosity=2)
