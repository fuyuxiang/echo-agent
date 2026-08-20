# 发布流程

Echo Agent 的版本发布流程。

---

## 版本策略

Echo Agent 使用 [SemVer](https://semver.org/) 语义化版本，当前处于 Beta 阶段（0.x.y）。

## 发布步骤

### 1. 准备

- [ ] 所有 CI 检查通过
- [ ] 更新 `CHANGELOG.md`
- [ ] 更新 `pyproject.toml` 版本号
- [ ] 确认文档与代码一致

### 2. 构建

```bash
# 构建 Dashboard
cd web && pnpm install --frozen-lockfile && pnpm build && cd ..

# 验证 Dashboard 产物
test -f web/dist/index.html

# 构建 Python 包
hatch build
```

### 3. 验证

```bash
# 在隔离环境安装测试
python -m venv /tmp/smoke
/tmp/smoke/bin/pip install dist/*.whl
/tmp/smoke/bin/echo-agent --version
/tmp/smoke/bin/echo-agent --help
```

### 4. 发布

```bash
hatch publish
```

### 5. 打 Tag

```bash
git tag v0.3.x
git push origin v0.3.x
```

## 自动化

`scripts/publish.sh` 封装了上述步骤 2-4。

## 发布检查清单

- [ ] Dashboard 已构建且包含在 wheel 中
- [ ] `echo_agent/_bundled/dashboard/index.html` 存在于产物中
- [ ] 安装后 `echo-agent --help` 正常运行
- [ ] 版本号正确
- [ ] CHANGELOG 已更新
- [ ] Git tag 已创建

### 文档站的版本标记

文档站不做多版本化（未使用 mike 等版本化插件），线上始终只有 master 的一份文档，`docs.yml` 在推送到 master 时自动重新部署。因此发布本身不需要额外的部署动作。

但有几处版本号是手写在正文里的，发版时需一并更新：

- `docs/index.md` 与 `docs/index.en.md` 的项目状态块
- `docs/operations/upgrade-migrations.md` 的「当前版本」与升级示例

改完这些页面的推送会触发 `docs.yml` 重新部署，无需手动干预。
