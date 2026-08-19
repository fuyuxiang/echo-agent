# Dashboard 开发

Echo Agent Dashboard 前端开发指南。

---

## 技术栈

- React 19
- TypeScript 5.7
- Vite 6（构建工具）
- Tailwind CSS 4
- Zustand 5（状态管理）
- React Router 8
- i18next（国际化）
- Recharts（图表）
- Vitest 3 + Testing Library（测试）

## 环境搭建

```bash
cd web
pnpm install --frozen-lockfile
pnpm dev    # 启动开发服务器（HMR）
```

!!! note
    需要 Node.js 24+ 和 pnpm 10+

## 目录结构

```
web/
├── src/
│   ├── components/     # 通用组件
│   ├── pages/          # 页面组件
│   ├── stores/         # Zustand 状态
│   ├── hooks/          # 自定义 Hooks
│   ├── i18n/           # 国际化资源
│   ├── api/            # API 客户端
│   └── utils/          # 工具函数
├── package.json
├── pnpm-lock.yaml
├── vite.config.ts
├── tailwind.config.ts
└── tsconfig.json
```

## 常用命令

```bash
pnpm dev          # 开发服务器
pnpm build        # 生产构建（tsc -b && vite build）
pnpm test --run   # 运行测试
pnpm preview      # 预览构建产物
```

## 与 Gateway 对接

Dashboard 通过以下方式与 Gateway 通信：

- HTTP REST API: `/api/v1/*`
- WebSocket: `/ws` (dashboard 管理流)

开发时默认 proxy 到 `http://127.0.0.1:58123`。

## 构建产物

构建后的 SPA 输出到 `web/dist/`，通过 hatch_build.py 打包进 wheel：

```
echo_agent/_bundled/dashboard/index.html
```

用户通过 `echo-agent dashboard build` 触发首次构建。

## 测试

```bash
pnpm test --run     # 单次运行
pnpm test           # watch 模式
```

使用 Vitest + @testing-library/react + jsdom 环境。
