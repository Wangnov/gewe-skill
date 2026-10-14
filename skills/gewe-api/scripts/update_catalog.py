#!/usr/bin/env python3
"""Generate the compact GeWe operation catalog from the official llms.txt."""

from __future__ import annotations

import argparse
import json
import re
import urllib.request
from pathlib import Path

INDEX_URL = "https://doc.geweapi.com/llms.txt"
API_RE = re.compile(
    r"^-\s+(?P<trail>.*?)\[(?P<title>[^]]+)\]"
    r"\((?P<url>https://doc\.geweapi\.com/(?P<id>api-[0-9]+)\.md)\)"
)

READ_PREFIXES = (
    "获取",
    "搜索",
    "下载",
    "检查",
    "同步",
    "自己的朋友圈列表",
    "联系人的朋友圈列表",
    "某条朋友圈详情",
    "用户主页",
    "关注列表",
    "消息列表",
    "评论列表",
)

READ_TITLES = {"标签列表", "cdn下载", "我的视频号二维码"}
MUTATION_EXCEPTIONS = {"搜索并关注"}

ALIASES = {
    "修改好友标签": ["打标签", "加标签", "好友打标签", "给好友加标签", "移除好友标签", "标签分组"],
    "标签列表": ["有哪些标签", "查看标签", "好友标签列表", "所有标签"],
    "删除群成员": ["踢人", "移出群聊", "群里删人"],
    "邀请/添加 进群": ["拉人进群", "邀请进群", "群里加人"],
    "发送文字消息": ["发微信", "发文本", "发送消息"],
    "设置好友备注": ["改好友备注", "修改联系人备注"],
    "修改群名称": ["改群名", "群聊改名"],
    "群保存到通讯录": ["保存群聊", "群加入通讯录"],
    "设置消息免打扰": ["群免打扰", "消息静音"],
    "检查是否在线": ["账号在线状态", "微信是否掉线"],
    "退出": ["退出登录", "微信下线"],
}


def risk_for(title: str) -> str:
    if title not in MUTATION_EXCEPTIONS and (
        title in READ_TITLES or title.startswith(READ_PREFIXES)
    ):
        return "read"
    return "mutation"


def generate(text: str) -> list[dict[str, object]]:
    operations: list[dict[str, object]] = []
    for line in text.splitlines():
        match = API_RE.match(line)
        if not match:
            continue
        trail = match.group("trail").strip()
        category = trail.split(">")[-1].strip() if trail else "API"
        title = match.group("title").strip()
        operations.append(
            {
                "id": match.group("id"),
                "title": title,
                "category": category,
                "doc_url": match.group("url"),
                "risk": risk_for(title),
                "aliases": ALIASES.get(title, []),
            }
        )
    return operations


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--input", type=Path)
    args = parser.parse_args()

    if args.input:
        text = args.input.read_text(encoding="utf-8")
    else:
        with urllib.request.urlopen(INDEX_URL, timeout=30) as response:
            text = response.read().decode("utf-8")

    operations = generate(text)
    if len(operations) < 100:
        raise SystemExit(f"refusing incomplete catalog: only {len(operations)} APIs")

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as handle:
        for operation in operations:
            handle.write(json.dumps(operation, ensure_ascii=False, separators=(",", ":")))
            handle.write("\n")
    print(json.dumps({"output": str(args.output), "count": len(operations)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
