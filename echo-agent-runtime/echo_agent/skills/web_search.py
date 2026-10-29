"""Shared search adapter for approved skill subprocesses.

This is not a native read-only tool: callers execute behind skill_run's
execution approval boundary. Native web tools retain their guarded transport.
"""

import sys
from urllib.parse import urlencode, urlparse
from urllib.request import Request, urlopen
import xml.etree.ElementTree as ET
from echo_agent.dependencies.skill_require import require  # noqa: E402


def _search_360(query: str, max_results: int) -> list[dict]:
    from lxml import html

    url = "https://www.so.com/s?" + urlencode({"q": query})
    request = Request(url, headers={"User-Agent": "Mozilla/5.0"})
    cap = 2 * 1024 * 1024
    with urlopen(request, timeout=10) as response:
        body = response.read(cap + 1)
    if len(body) > cap:
        raise ValueError("360 search response exceeds 2 MiB")
    root = html.fromstring(body.decode("utf-8", errors="replace"))
    for element in root.xpath("//script|//style"):
        element.drop_tree()
    anchors = root.xpath('//li[contains(concat(" ", normalize-space(@class), " "), " res-list ")]//h3//a[@href]')
    results, seen = [], set()
    for anchor in anchors:
        href = anchor.get("data-mdurl") or anchor.get("data-initurl") or anchor.get("href", "")
        if (not href.startswith(("https://", "http://")) or href in seen
                or urlparse(href).hostname in {"ai.so.com", "map.360.cn"}
                or urlparse(anchor.get("href", "")).hostname == "ai.so.com"):
            continue
        title = " ".join("".join(anchor.itertext()).split())
        if not title:
            continue
        parent = anchor.xpath("ancestor::li[1]")[0]
        snippet = " ".join(" ".join(parent.itertext()).split())
        if snippet.startswith(title):
            snippet = snippet[len(title):].strip()
        results.append({"title": title, "href": href, "body": snippet[:1000]})
        seen.add(href)
        if len(results) >= max_results:
            break
    return results


def _search_bing_rss(query: str, max_results: int, region: str) -> list[dict]:
    # Bing's RSS response works on hosts where the HTML search endpoint
    # returns an empty page or a bot challenge. Bound the download and timeout.
    host = "cn.bing.com" if region.startswith("cn") else "www.bing.com"
    url = f"https://{host}/search?" + urlencode({"q": query, "format": "rss"})
    request = Request(url, headers={"User-Agent": "Mozilla/5.0"})
    cap = 2 * 1024 * 1024
    with urlopen(request, timeout=10) as response:
        body = response.read(cap + 1)
    if len(body) > cap:
        raise ValueError("Bing RSS response exceeds 2 MiB")
    root = ET.fromstring(body)
    if root.tag != "rss":
        raise ValueError("Search endpoint did not return RSS")
    results = []
    for item in root.findall("./channel/item"):
        href = (item.findtext("link") or "").strip()
        if not href.startswith(("https://", "http://")):
            continue
        results.append({"title": (item.findtext("title") or "").strip(),
                        "href": href,
                        "body": (item.findtext("description") or "").strip()})
        if len(results) >= max_results:
            break
    return results


def search_text(query: str, max_results: int = 5, region: str = "wt-wt"):
    max_results = max(1, min(max_results, 20))
    if region.startswith("cn") or any("\u3400" <= char <= "\u9fff" for char in query):
        try:
            results = _search_360(query, max_results)
            if results:
                return results
        except Exception as exc:
            print(f"360 search unavailable; trying Bing RSS: {exc}", file=sys.stderr)
    try:
        results = _search_bing_rss(query, max_results, region)
        if results:
            return results
    except Exception as exc:
        print(f"Bing RSS unavailable; trying DDGS: {exc}", file=sys.stderr)
    require("skill.web-search")
    from ddgs import DDGS

    try:
        results = DDGS(timeout=15).text(query, max_results=max_results, region=region)
    except Exception as exc:
        raise RuntimeError(f"Search failed after public-source fallbacks: {exc}") from exc
    if not results:
        raise RuntimeError("Search returned no usable evidence after public-source fallbacks")
    return results


def search_news(query: str, max_results: int = 5):
    require("skill.web-search")
    from ddgs import DDGS

    return DDGS(timeout=15).news(query, max_results=max_results)


def search_images(query: str, max_results: int = 5):
    require("skill.web-search")
    from ddgs import DDGS

    return DDGS(timeout=15).images(query, max_results=max_results)
