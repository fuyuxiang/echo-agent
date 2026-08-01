# tests/test_dead_injected_attr_guard.py
"""守卫:构造参数存进 self._x 后从未被读 —— "配置假生效"的真实成因。

配置治理已有 test_config_metadata_guard 把住 status/ref 元数据,但那层只能校验 ref
指向的文件存在。真正让 max_tree_depth 这类字段"标着 effective 却毫无效果"的机制是:
值从 config 正确读出、正确传进构造器、存成 self._x,然后再没有人读它。文件存在、字段
名也在文件里,元数据守卫全绿,而功能是死的。

本守卫用 AST 补上这一层:注入即遗弃的私有属性必须显式登记在白名单里。新增一处就会
失败,迫使作者要么接线,要么按治理规矩标 dead + disposition。

口径说明(有意从宽,换取零误报):
- 只看私有属性(_x)。公开属性可能由外部读取,判不了。
- "被读"按全仓统计:任一处出现 self._x 读取即算。这样父类属性被子类读、mixin 读都
  不会误报,代价是同名属性会互相掩护 —— 对回归闸门而言这个方向的偏差是安全的。
- 识别 getattr(self, "_x", ...) 动态读取,否则 InferenceStage._memory_store 会误报。
"""
from __future__ import annotations

import ast
from pathlib import Path

_ECHO_ROOT = Path(__file__).resolve().parent.parent / "echo_agent"

# 已知的注入即遗弃属性。每一项都是欠债,不是豁免:修掉一个就从这里删掉一行。
# 允许列表只能变短,变长必须在评审里说明理由。
_KNOWN_DEAD: set[tuple[str, str, str]] = {
    # worker 不经 model_router 选模型,恒用 model or profile.model or default_model。
    # 接线需要先定 worker 的 task_type 约定(主链取自 ctx.task_type,worker 无此上下文)。
    ("agent/multi_agent/runtime.py", "WorkerExecutor", "_model_router"),
    ("agent/tools/delegate.py", "DelegateTool", "_model_router"),
    # 冗余存储:真值在 __init__ 内以局部变量传给了 EvolutionGate,行为正确。
    ("evolution/engine.py", "EvolutionEngine", "_eval_dataset_loader"),
    # 池化包装器只用 config 造实例,_cls 存了没用到。
    ("models/providers/__init__.py", "_PooledProvider", "_cls"),
    # planning.maxTreeDepth 已按治理规矩标 dead + disposition=fix(schema.py),
    # 待深度语义实现后一并接线。
    ("agent/planning/planner.py", "AgentPlanner", "_max_tree_depth"),
}


def _collect_read_attr_names(trees: dict[Path, ast.Module]) -> set[str]:
    """全仓被读过的 self 属性名(含 getattr 动态读取与 del)。"""
    names: set[str] = set()
    for tree in trees.values():
        for node in ast.walk(tree):
            if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
                if node.value.id == "self" and isinstance(node.ctx, (ast.Load, ast.Del)):
                    names.add(node.attr)
            # getattr(self, "_x") / hasattr(self, "_x") 也算读取
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                if node.func.id in ("getattr", "hasattr") and len(node.args) >= 2:
                    target, attr = node.args[0], node.args[1]
                    if (isinstance(target, ast.Name) and target.id == "self"
                            and isinstance(attr, ast.Constant) and isinstance(attr.value, str)):
                        names.add(attr.value)
    return names


def _find_dead_injected_attrs() -> set[tuple[str, str, str]]:
    trees: dict[Path, ast.Module] = {}
    for path in sorted(_ECHO_ROOT.rglob("*.py")):
        try:
            trees[path] = ast.parse(path.read_text(encoding="utf-8"))
        except SyntaxError:  # pragma: no cover - 语法错误由其它检查负责
            continue

    read_names = _collect_read_attr_names(trees)
    found: set[tuple[str, str, str]] = set()
    for path, tree in trees.items():
        rel = path.relative_to(_ECHO_ROOT).as_posix()
        for cls in [n for n in ast.walk(tree) if isinstance(n, ast.ClassDef)]:
            init = next((f for f in cls.body
                         if isinstance(f, ast.FunctionDef) and f.name == "__init__"), None)
            if init is None:
                continue
            params = ({a.arg for a in init.args.args[1:]}
                      | {a.arg for a in init.args.kwonlyargs})
            for node in ast.walk(init):
                if not isinstance(node, ast.Assign) or not isinstance(node.value, ast.Name):
                    continue
                if node.value.id not in params:
                    continue
                for target in node.targets:
                    if (isinstance(target, ast.Attribute)
                            and isinstance(target.value, ast.Name)
                            and target.value.id == "self"
                            and target.attr.startswith("_")
                            and target.attr not in read_names):
                        found.add((rel, cls.name, target.attr))
    return found


def test_no_new_dead_injected_attrs():
    found = _find_dead_injected_attrs()
    new = found - _KNOWN_DEAD
    assert not new, (
        "发现新的'注入即遗弃'属性 —— 构造参数存进 self 后从未被读,"
        f"这正是配置假生效的成因:{sorted(new)}\n"
        "请接线消费它,或按治理规矩把对应配置标 dead + disposition。"
    )


def test_known_dead_list_has_no_stale_entries():
    """白名单只能变短:修好了就必须删掉对应行,否则它会掩护未来的同名回归。"""
    found = _find_dead_injected_attrs()
    stale = _KNOWN_DEAD - found
    assert not stale, f"白名单存在已修复条目,请删除:{sorted(stale)}"
