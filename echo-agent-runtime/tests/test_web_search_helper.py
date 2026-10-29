import io
from types import SimpleNamespace

import pytest


@pytest.fixture
def helper():
    from echo_agent.skills import web_search
    return web_search


def test_rss_parsing_and_query_encoding(helper, monkeypatch):
    body = b'<rss><channel><item><title>A</title><link>https://a.example/</link><description>text</description></item><item><title>B</title><link>https://b.example/</link></item></channel></rss>'
    seen = []
    def fake_open(req, timeout):
        seen.append((req.full_url, timeout))
        return io.BytesIO(body)
    monkeypatch.setattr(helper, 'urlopen', fake_open)
    result = helper._search_bing_rss('a & b', 1, 'cn-zh')
    assert result == [{'title': 'A', 'href': 'https://a.example/', 'body': 'text'}]
    assert 'cn.bing.com/search?q=a+%26+b&format=rss' in seen[0][0]
    assert seen[0][1] == 10


@pytest.mark.parametrize('body', [b'<html>challenge</html>', b'bad xml', b'x' * (2 * 1024 * 1024 + 1)])
def test_invalid_or_oversized_search_response_is_not_a_result(helper, monkeypatch, body):
    monkeypatch.setattr(helper, 'urlopen', lambda *a, **k: io.BytesIO(body))
    with pytest.raises((ValueError, helper.ET.ParseError)):
        helper._search_bing_rss('query', 5, 'wt-wt')


def test_successful_rss_does_not_need_ddgs(helper, monkeypatch):
    expected = [{'title': 'A', 'href': 'https://a.example/', 'body': 'text'}]
    monkeypatch.setattr(helper, '_search_bing_rss', lambda *a: expected)
    def forbidden(*a):
        raise AssertionError('No package install needed for RSS')
    monkeypatch.setattr(helper, 'require', forbidden)
    assert helper.search_text('query') == expected


def test_search_falls_back_to_new_ddgs_package(helper, monkeypatch):
    import sys
    monkeypatch.setattr(helper, '_search_360', lambda *a: [])
    monkeypatch.setattr(helper, '_search_bing_rss', lambda *a: [])
    monkeypatch.setattr(helper, 'require', lambda *a: None)
    calls = []
    def fake_text(query, **kwargs):
        calls.append((query, kwargs))
        return [{'title': 'fallback', 'href': 'https://example.com'}]
    monkeypatch.setitem(sys.modules, 'ddgs', SimpleNamespace(DDGS=lambda **kw: SimpleNamespace(text=fake_text)))
    assert helper.search_text('query', 3, 'cn-zh')[0]['title'] == 'fallback'
    assert calls == [('query', {'max_results': 3, 'region': 'cn-zh'})]


def test_chinese_query_uses_relevant_chinese_search_source(helper, monkeypatch):
    expected = [{'title': '朝阳美食', 'href': 'https://example.com/food', 'body': '北京朝阳区餐厅'}]
    monkeypatch.setattr(helper, '_search_360', lambda *a: expected)
    monkeypatch.setattr(helper, '_search_bing_rss', lambda *a: [])
    assert helper.search_text('北京 朝阳区 美食') == expected


def test_chinese_html_parser_prefers_original_source_links(helper, monkeypatch):
    page = '''<html><li class="res-list"><h3><a href="https://www.so.com/link?id=1" data-mdurl="https://source.example/food">朝阳<em>美食</em></a></h3><p>餐厅介绍</p></li><li class="res-list"><h3><a href="https://ai.so.com/answer">AI answer</a></h3></li></html>'''
    monkeypatch.setattr(helper, 'urlopen', lambda *a, **k: io.BytesIO(page.encode()))
    result = helper._search_360('朝阳 美食', 3)
    assert len(result) == 1
    assert result[0]['href'] == 'https://source.example/food'
    assert '美食' in result[0]['title'] and '餐厅介绍' in result[0]['body']


def test_chinese_page_strips_css_scripts_and_cards(helper, monkeypatch):
    page = '''<li class="res-list"><h3><a href="https://original.example/">朝阳美食</a></h3>
    <style>.junk {color:red}</style><script>untrusted()</script><p>餐厅和烤鸭</p></li>
    <li class="res-list"><h3><a href="https://map.360.cn/">地图</a></h3></li>'''
    monkeypatch.setattr(helper, 'urlopen', lambda *a, **k: io.BytesIO(page.encode()))
    results = helper._search_360('北京 朝阳区 美食', 5)
    assert len(results) == 1
    assert '烤鸭' in results[0]['body']
    assert 'color' not in results[0]['body'] and 'untrusted' not in results[0]['body']


@pytest.mark.parametrize('page', [b'<html>security challenge</html>', b'<html>No results</html>'])
def test_chinese_challenge_is_not_a_search_result(helper, monkeypatch, page):
    monkeypatch.setattr(helper, 'urlopen', lambda *a, **k: io.BytesIO(page))
    assert helper._search_360('query', 5) == []


def test_timeouts_and_challenge_fall_back_once(helper, monkeypatch):
    def timeout(*a, **k):
        raise TimeoutError('search timed out')
    monkeypatch.setattr(helper, 'urlopen', timeout)
    monkeypatch.setattr(helper, 'require', lambda _: None)
    import sys
    monkeypatch.setitem(sys.modules, 'duckduckgo_search', None)
    monkeypatch.setitem(sys.modules, 'ddgs', SimpleNamespace(DDGS=lambda **kw: SimpleNamespace(
        text=lambda *a, **k: [{'title': '朝阳餐厅', 'href': 'https://food.example/', 'body': '烤鸭'}])))
    assert helper.search_text('北京 朝阳区 美食')[0]['title'] == '朝阳餐厅'


def test_deep_research_reuses_adapter_with_only_new_package(helper, monkeypatch):
    import importlib.util
    import sys
    from pathlib import Path
    path = Path(__file__).resolve().parents[1] / 'skills/research/deep-research/scripts/research_report.py'
    spec = importlib.util.spec_from_file_location('research_report', path)
    report = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(report)
    assert report.search_text is helper.search_text
    monkeypatch.setitem(sys.modules, 'duckduckgo_search', None)
    monkeypatch.setattr(helper, '_search_bing_rss', lambda *a: [])
    monkeypatch.setattr(helper, 'require', lambda _: None)
    monkeypatch.setitem(sys.modules, 'ddgs', SimpleNamespace(DDGS=lambda **kw: SimpleNamespace(
        text=lambda *a, **k: [{'title': 'source', 'href': 'https://source.example/', 'body': 'text'}])))
    assert report.search_multi('test') == [{'title': 'source', 'href': 'https://source.example/', 'body': 'text'}]


def test_all_empty_sources_report_no_evidence(helper, monkeypatch):
    import sys
    monkeypatch.setattr(helper, '_search_360', lambda *a: [])
    monkeypatch.setattr(helper, '_search_bing_rss', lambda *a: [])
    monkeypatch.setattr(helper, 'require', lambda _: None)
    monkeypatch.setitem(sys.modules, 'ddgs', SimpleNamespace(DDGS=lambda **kw: SimpleNamespace(text=lambda *a, **k: [])))
    with pytest.raises(RuntimeError, match='no usable evidence'):
        helper.search_text('北京 朝阳区 美食')


def test_chinese_oversized_response_is_rejected(helper, monkeypatch):
    monkeypatch.setattr(helper, 'urlopen', lambda *a, **k: io.BytesIO(b'x' * (2 * 1024 * 1024 + 1)))
    with pytest.raises(ValueError, match='exceeds 2 MiB'):
        helper._search_360('query', 5)
