# DOCX preview

This private pnpm workspace package builds the TypeScript renderer from
[`apollocmh/docx-preview` v0.9.0](https://github.com/apollocmh/docx-preview/tree/18045ccc783e706078b8587febc043f9f1ed9449/packages/docx-preview),
commit `18045ccc783e706078b8587febc043f9f1ed9449`. It retains the complete core
renderer, including the parser, paginator and encrypted-DOCX APIs. See the
original [LICENSE](LICENSE) and [NOTICE](NOTICE).

The [release tarball](https://github.com/apollocmh/docx-preview/releases/download/v0.9.0/docx-preview-0.9.0.tgz)
matches the previous pnpm lockfile integrity
`sha512-tbCwbV4v5T4pvOLY2xS4HhBNA1jXAsmemiXbk18CfsoVoTCSfKuUbLQHfclciAUTAEAIHySlq4CpidVgcPEiQg==`.
Its `.mjs`, `.cjs` and `.js` Git blob hashes matched the original patch inputs.
The source files were fetched from that commit and checked against its Git tree.
[`api.d.ts`](api.d.ts) is the declaration from the verified tarball. Vite bundles
[`src/docx-preview.ts`](src/docx-preview.ts) via the normal lazy import.
The two Noble dependencies use the same versions and integrities as upstream.

EchoAgent maintains two source changes. `html-renderer.ts` applies section
breaks before the incoming section and explicit page breaks after their
content. `docx-preview.ts` waits for the rendering iframe's fonts before layout.
The packaging script stages the Apache license, the NOTICE and the Noble
licenses alongside the other local preview assets.

Run `pnpm --filter @apollo-design/docx-preview typecheck`,
`pnpm exec vitest run src/lib/__tests__/word-preview-pagination.test.ts`, and
`pnpm test:ui:office-workbench` from the repository root after an update. Compare
`api.d.ts` with the candidate release and reapply only the source changes still
needed for the preview behavior.
