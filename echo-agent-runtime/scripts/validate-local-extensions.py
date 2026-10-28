"""Run disposable real stdio MCP and local plugin lifecycle checks.

Usage: python scripts/validate-local-extensions.py --output /tmp/echo-extensions
Requires an installed Echo Agent; no model credentials or external servers.
"""

import asyncio
import json
import sys
import argparse
from pathlib import Path
from echo_agent.mcp.client import MCPClient
from echo_agent.mcp.transport import StdioTransport
from echo_agent.plugins.manager import PluginManager
from echo_agent.config.schema import Config
from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.queue import MessageBus


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--output", type=Path, required=True)
r = parser.parse_args().output.resolve()
if r.exists() and any(r.iterdir()):
    parser.error("--output must be a new or empty directory")
r.mkdir(parents=True, exist_ok=True)
server = r / "mcp-server.py"
server.write_text(
    """import sys,json,time\nfor line in sys.stdin:\n q=json.loads(line);method=q.get('method');rid=q.get('id')\n if rid is None:continue\n if method=='initialize':b={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'qa-local','version':'1'}}\n elif method=='tools/list':b={'tools':[{'name':'qa_sum','description':'Sum numbers','inputSchema':{'type':'object','properties':{'a':{'type':'number'},'b':{'type':'number'}},'required':['a','b']}}]}\n elif method=='tools/call':\n  p=q['params'];a=p.get('arguments',{});b={'content':[{'type':'text','text':str(a['a']+a['b'])}]}\n else:b={}\n print(json.dumps({'jsonrpc':'2.0','id':rid,'result':b}),flush=True)\n"""
)
p = r / "plugins/qa-local-plugin"
p.mkdir(parents=True, exist_ok=True)
(p / "plugin.yaml").write_text("name: qa-local-plugin\nversion: '1.0.0'\npermissions: [tool.register]\n")
(p / "__init__.py").write_text(
    """from echo_agent.tools import Tool,ToolResult\nclass QATool(Tool):\n name='qa_plugin_echo'\n description='Return synthetic QA input'\n parameters={'type':'object','properties':{'text':{'type':'string'}},'required':['text']}\n risk_level='read_only'\n async def execute(self,params,ctx=None):return ToolResult(output=params['text'])\nasync def activate(ctx):ctx.register_tool(QATool())\nplugin={'activate':activate}\n"""
)


async def main():
    results = []
    for attempt in range(2):
        client = MCPClient("qa-local", StdioTransport(sys.executable, [str(server)]))
        await client.connect(timeout=10)
        tools = await client.list_tools()
        result = await client.call_tool("qa_sum", {"a": 19, "b": 23})
        assert result["content"][0]["text"] == "42", result
        await client.disconnect()
        assert not client.is_connected
        results.append(
            {
                "name": "mcp-stdio-" + str(attempt),
                "tools": tools,
                "result": result,
                "disconnected": not client.is_connected,
            }
        )
    cfg = Config()
    cfg.plugins.trusted_plugins = ["qa-local-plugin"]
    cfg.plugins.permission_mode = "strict"
    reg = ToolRegistry()
    manager = PluginManager(config=cfg, workspace=r, bus=MessageBus(), tool_registry=reg)
    await manager.discover_and_load()
    tool = reg.get("qa_plugin_echo")
    assert tool is not None, [(x.manifest.name, x.status, x.error) for x in manager.plugins]
    result = await tool.execute({"text": "PLUGIN-QA-58"})
    assert result.output == "PLUGIN-QA-58"
    results.append(
        {
            "name": "plugin-real-module",
            "status": [(x.manifest.name, x.status) for x in manager.plugins],
            "result": result.output,
        }
    )
    await manager.shutdown()
    assert reg.get("qa_plugin_echo") is None
    results.append({"name": "plugin-unload", "removed": True})
    (r / "results.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results))


asyncio.run(main())
