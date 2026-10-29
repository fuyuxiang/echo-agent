import {
  Component,
  memo,
  useEffect,
  useMemo,
  useState,
  type ErrorInfo,
  type ReactNode,
} from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import { common } from "lowlight";
import { LinkifyIt } from "linkify-it";
import "katex/dist/katex.min.css";

import { preprocessMarkdown } from "./preprocess";
import { remarkCodeLanguage } from "./plugins/remark-code-language";
import { remarkLinkifyIt } from "./plugins/remark-linkify-it";
import { rehypeCodeBlock } from "./plugins/rehype-code-block";
import { rehypeInlineCode } from "./plugins/rehype-inline-code";
import { rehypeFixAutolinkBoundary } from "./plugins/rehype-fix-autolink-boundary";
import { rehypeFindHighlight } from "./plugins/rehype-find-highlight";
import { MarkdownPre } from "./MarkdownPre";
import { MarkdownPreMermaid } from "./MarkdownPreMermaid";
import { MarkdownPreviewImage } from "./MarkdownPreviewImage";
import { MarkdownInlineCode } from "./MarkdownInlineCode";
import type { MarkdownConfig, MarkdownProps } from "./types";

/* ---------- sanitize schema (hljs + katex/mathml) ---------- */

const MAX_INLINE_IMAGE_URL_LENGTH = 12 * 1024 * 1024;
const INLINE_RASTER_DATA_URL = /^data:image\/(?:png|jpe?g|gif|webp);base64,/i;

function isSafeInlineRasterUrl(value: string | undefined): boolean {
  return Boolean(
    value
      && value.length <= MAX_INLINE_IMAGE_URL_LENGTH
      && INLINE_RASTER_DATA_URL.test(value),
  );
}

function buildSanitizeSchema(config?: MarkdownConfig) {
  const schema = { ...defaultSchema };
  schema.attributes = {
    ...defaultSchema.attributes,
    code: [...(defaultSchema.attributes?.code || []), "className", "meta"],
    // GFM 任务列表的 checkbox 需要 `checked`（defaultSchema 会剥掉它，
    // 导致 [x]/[ ] 渲染成一模一样的未勾选框）。
    input: ["type", "checked", "disabled"],
    span: ["className", "style", "ariaHidden", "role"],
    div: ["className", "style"],
    pre: [...(defaultSchema.attributes?.pre || []), "className"],
    img: [...(defaultSchema.attributes?.img || []), "alt", "title"],
    "*": [
      "className",
      "style",
      "ariaHidden",
      "role",
      "tabIndex",
      "id",
      "title",
    ],
    math: ["xmlns", "display", "alttext"],
    semantics: ["*"],
    annotation: ["encoding"],
    mrow: ["*"],
    msup: ["*"],
    msub: ["*"],
    msubsup: ["*"],
    mi: ["mathvariant"],
    mn: ["*"],
    mo: ["stretchy", "fence", "separator", "lspace", "rspace", "form"],
    mfrac: ["linethickness"],
    msqrt: ["*"],
    mroot: ["*"],
    munder: ["*"],
    mover: ["*"],
    munderover: ["*"],
    mtable: ["*"],
    mtr: ["*"],
    mtd: ["*"],
    mspace: ["width", "height", "depth"],
    mtext: ["*"],
    mstyle: ["*"],
    mpadded: ["*"],
    mphantom: ["*"],
  };
  schema.tagNames = [
    ...(defaultSchema.tagNames || []),
    "mark",
    "math",
    "semantics",
    "mrow",
    "msup",
    "mi",
    "mn",
    "mo",
    "mfrac",
    "msqrt",
    "mroot",
    "msubsup",
    "msub",
    "munder",
    "mover",
    "munderover",
    "mtable",
    "mtr",
    "mtd",
    "mspace",
    "mtext",
    "mstyle",
    "mpadded",
    "mphantom",
    "annotation",
    "maligngroup",
    "malignmark",
    "menclose",
    "merror",
    "mfenced",
    "mglyph",
    "mlabeledtr",
    "mlongdiv",
    "mmultiscripts",
    "mstack",
    "mscarries",
    "mscarry",
    "msgroup",
    "msline",
    "msrow",
    "maction",
  ];

  const extraSchemes = config?.customUrlSchemes?.map((s) => s.scheme) ?? [];
  schema.protocols = {
    ...defaultSchema.protocols,
    href: [...(defaultSchema.protocols?.href || []), ...extraSchemes],
    // `data:` is admitted by the syntax sanitizer, then constrained to a
    // small allow-list of raster MIME types and a hard length cap by the
    // image component below. SVG data URLs remain blocked.
    src: [...(defaultSchema.protocols?.src || []), "data"],
  };
  if (config?.imageUrlResolver || config?.imageUrlResolverAsync) {
    schema.protocols = {
      ...schema.protocols,
      src: [
        ...(schema.protocols.src || []),
        "file",
        "local-file",
        "blob",
      ],
    };
  }
  return schema;
}

/* ---------- error boundary ---------- */

const MAX_ERROR_RETRIES = 1;

type BoundaryProps = {
  fallbackText: string;
  remarkPlugins: unknown[];
  rehypePlugins: unknown[];
  components: Record<string, unknown>;
  urlTransform: (url: string) => string;
  children: ReactNode;
};

type BoundaryState = {
  hasError: boolean;
  errorCount: number;
  retryKey: number;
};

class MarkdownErrorBoundary extends Component<BoundaryProps, BoundaryState> {
  state: BoundaryState = { hasError: false, errorCount: 0, retryKey: 0 };

  static getDerivedStateFromError() {
    return { hasError: true };
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    this.setState((prev) => ({ errorCount: prev.errorCount + 1 }));
    console.warn("[Markdown] DOM reconciliation error caught:", error.message);
  }

  componentDidUpdate(prevProps: BoundaryProps) {
    if (this.state.hasError && prevProps.fallbackText !== this.props.fallbackText) {
      this.setState((prev) => ({
        hasError: false,
        errorCount: 0,
        retryKey: prev.retryKey + 1,
      }));
    }
  }

  render() {
    if (this.state.hasError) {
      if (this.state.errorCount > MAX_ERROR_RETRIES) {
        return <pre className="md-fallback">{this.props.fallbackText}</pre>;
      }
      return (
        <ReactMarkdown
          key={`fallback-${this.state.retryKey}`}
          remarkPlugins={this.props.remarkPlugins as never}
          rehypePlugins={this.props.rehypePlugins as never}
          components={this.props.components as never}
          urlTransform={this.props.urlTransform}
        >
          {this.props.fallbackText}
        </ReactMarkdown>
      );
    }
    return <div key={this.state.retryKey}>{this.props.children}</div>;
  }
}

/* ---------- image with async resolver ---------- */

function MarkdownImage({
  src,
  alt,
  config,
  ...imgProps
}: {
  src?: string;
  alt?: string;
  config?: MarkdownConfig;
} & React.ImgHTMLAttributes<HTMLImageElement>) {
  // Model-authored Markdown is untrusted. Rendering a remote image directly
  // would issue a privacy-sensitive GET as soon as the message becomes
  // visible (tracking pixels and local-network probes do not need CORS).
  // Only bounded inline raster data, or a URL produced by an explicit host
  // resolver, may become an <img> source automatically.
  const hasResolver = Boolean(config?.imageUrlResolver || config?.imageUrlResolverAsync);
  const syncResolved = src && config?.imageUrlResolver
    ? config.imageUrlResolver(src)
    : !hasResolver
      ? src
      : undefined;
  const [resolvedSrc, setResolvedSrc] = useState(syncResolved);

  useEffect(() => {
    let cancelled = false;
    setResolvedSrc(syncResolved);
    if (!src || !config?.imageUrlResolverAsync) {
      return () => {
        cancelled = true;
      };
    }
    config
      .imageUrlResolverAsync(src)
      .then((asyncResolved) => {
        if (!cancelled && asyncResolved) setResolvedSrc(asyncResolved);
      })
      .catch((error) => {
        config.onImageResolveError?.(error, { src });
      });
    return () => {
      cancelled = true;
    };
  }, [config, src, syncResolved]);

  const safeInlineRaster = isSafeInlineRasterUrl(resolvedSrc);
  const safeResolvedResource = hasResolver
    && resolvedSrc != null
    && (/^(?:blob:|asset:|local-file:)/i.test(resolvedSrc)
      || /^https:\/\/asset\.localhost(?:\/|$)/i.test(resolvedSrc));

  if ((safeInlineRaster || safeResolvedResource) && resolvedSrc) {
    return <MarkdownPreviewImage {...imgProps} src={resolvedSrc} alt={alt} />;
  }

  const remote = src != null && /^https?:\/\//i.test(src);
  return (
    <span className="md-remote-image-blocked" role="note">
      <span>已阻止自动加载远程图片</span>
      {remote ? (
        <a
          href={src}
          target="_blank"
          rel="noopener noreferrer"
          onClick={(event) => {
            if (
              config?.onLinkClick?.({
                href: src,
                children: alt,
                event,
              }) === false
            ) {
              event.preventDefault();
              event.stopPropagation();
            }
          }}
        >
          {alt?.trim() || "在浏览器中打开图片"}
        </a>
      ) : alt?.trim() ? (
        <span>{alt}</span>
      ) : null}
    </span>
  );
}

/* ---------- main renderer ---------- */

function MarkdownInner({
  children,
  complete = true,
  markdownTheme = "legacy",
  config,
  theme = "light",
  findQuery,
}: MarkdownProps) {
  const preprocessed = useMemo(
    () => preprocessMarkdown(children ?? ""),
    [children],
  );

  const sanitizeSchema = useMemo(() => buildSanitizeSchema(config), [config]);

  const linkify = useMemo(() => {
    const instance = new LinkifyIt({ fuzzyLink: false, fuzzyIP: false });
    config?.customUrlSchemes?.forEach((s) => {
      if (!s.linkifyValidate) return;
      instance.add(`${s.scheme}:`, { validate: s.linkifyValidate });
    });
    return instance;
  }, [config?.customUrlSchemes]);

  const urlTransform = useMemo(() => {
    const prefixes = (config?.customUrlSchemes ?? []).map(
      (scheme) => `${scheme.scheme.toLowerCase()}:`,
    );
    return (value: string) => {
      if (isSafeInlineRasterUrl(value)) return value;
      const lowerValue = value.toLowerCase();
      return prefixes.some((prefix) => lowerValue.startsWith(prefix))
        ? value
        : defaultUrlTransform(value);
    };
  }, [config?.customUrlSchemes]);

  const remarkPlugins = useMemo(
    () => [
      remarkGfm,
      remarkBreaks,
      [remarkMath, { singleDollarTextMath: true }] as const,
      [remarkLinkifyIt, { linkify }] as const,
      remarkCodeLanguage,
    ],
    [linkify],
  );

  /**
   * Plugin order matters:
   * 1. sanitize before katex (katex emits MathML + styles)
   * 2. code-block / inline-code before highlight (extract raw content)
   * 3. highlight last
   * 4. find-highlight runs AFTER sanitize so <mark> isn't stripped; sanitize
   *    schema 已显式允许 mark + className(find-hit / find-hit-active)。
   */
  const rehypePlugins = useMemo(
    () => {
      const plugins: unknown[] = [
        [rehypeSanitize, sanitizeSchema] as const,
        rehypeFixAutolinkBoundary,
        rehypeCodeBlock,
        rehypeInlineCode,
        [
          rehypeKatex,
          {
            strict: false,
            throwOnError: false,
            errorColor: "#cc0000",
            macros: {
              "\\RR": "\\mathbb{R}",
              "\\NN": "\\mathbb{N}",
              "\\ZZ": "\\mathbb{Z}",
              "\\QQ": "\\mathbb{Q}",
              "\\CC": "\\mathbb{C}",
            },
          },
        ] as const,
        [rehypeHighlight, { languages: { ...common } }] as const,
      ];
      // 会话内查找:把命中处包成 <mark>。query 为空时插件内部短路,无开销。
      if (findQuery) {
        plugins.push([
          rehypeFindHighlight,
          { query: findQuery },
        ] as const);
      }
      return plugins;
    },
    [sanitizeSchema, findQuery],
  );

  const components = useMemo(
    () => ({
      pre: ({
        children: preChildren,
        node,
        ...preProps
      }: {
        children?: ReactNode;
        node?: { data?: Record<string, unknown> };
      } & React.HTMLAttributes<HTMLPreElement>) => {
        const language = node?.data?.language as string | undefined;
        const content = node?.data?.content as string | undefined;
        const meta = node?.data?.meta as string | undefined;

        if (config?.staticRendering) {
          return <div className="md-code-wrapper" data-export-language={language || undefined}>
            <div className="md-code-container">
              {language && <div className="md-code-header"><span className="md-code-lang">{language}</span></div>}
              <pre className="md-code-pre">{preChildren}</pre>
            </div>
          </div>;
        }

        if (language === "mermaid") {
          return (
            <MarkdownPreMermaid
              content={content}
              complete={complete}
              language={language}
              theme={theme}
              onDownloadMermaid={config?.onDownloadMermaid}
              onPreviewMermaid={config?.onPreviewMermaid}
              codeBlockActions={config?.codeBlockActions}
              requestId={config?.requestId}
              onCodeBlockAction={config?.onCodeBlockAction}
              onApplyCode={config?.onApplyCode}
              expandThreshold={config?.expandThreshold}
            >
              {preChildren}
            </MarkdownPreMermaid>
          );
        }

        if (language === "latex") {
          return (
            <MarkdownPre
              {...preProps}
              language={language}
              code={content}
              meta={meta}
              pathClickHandler={config?.pathClickHandler}
              codeBlockActions={config?.codeBlockActions}
              onApplyCode={config?.onApplyCode}
              isLatex
              requestId={config?.requestId}
              onCodeBlockAction={config?.onCodeBlockAction}
            >
              {preChildren}
            </MarkdownPre>
          );
        }

        return (
          <MarkdownPre
            {...preProps}
            language={language}
            code={content}
            meta={meta}
            pathClickHandler={config?.pathClickHandler}
            codeBlockActions={config?.codeBlockActions}
            onApplyCode={config?.onApplyCode}
            requestId={config?.requestId}
            onCodeBlockAction={config?.onCodeBlockAction}
          >
            {preChildren}
          </MarkdownPre>
        );
      },
      table: ({
        children: tableChildren,
        ...tableProps
      }: React.TableHTMLAttributes<HTMLTableElement> & {
        children?: ReactNode;
      }) => (
        <div className="md-table-wrapper">
          <table {...tableProps}>{tableChildren}</table>
        </div>
      ),
      code: ({
        children: codeChildren,
        node,
        className,
        ...codeProps
      }: {
        children?: ReactNode;
        node?: { data?: { inline?: boolean } };
        className?: string;
      } & React.HTMLAttributes<HTMLElement>) => {
        if (!node?.data?.inline) {
          return (
            <code className={className} {...codeProps}>
              {codeChildren}
            </code>
          );
        }
        return (
          <MarkdownInlineCode
            pathClickHandler={config?.pathClickHandler}
            resolveCode={config?.resolveCode}
            openCodeLink={config?.openCodeLink}
            requestId={config?.requestId}
            renderPathIcon={config?.renderInlineCodePathIcon}
            className={className}
            {...codeProps}
          >
            {codeChildren}
          </MarkdownInlineCode>
        );
      },
      a: ({
        href,
        title,
        children: aChildren,
        ...aProps
      }: React.AnchorHTMLAttributes<HTMLAnchorElement> & {
        children?: ReactNode;
      }) => {
        if (config?.staticRendering && !/^(?:https?:|mailto:|#)/i.test(href ?? "")) {
          return <span>{aChildren}</span>;
        }
        const handleClick = (e: React.MouseEvent<HTMLAnchorElement>) => {
          if (config?.onLinkClick) {
            if (
              config.onLinkClick({
                href: href || "",
                title,
                children: aChildren,
                event: e,
              }) === false
            ) {
              e.preventDefault();
              e.stopPropagation();
            }
          }
        };
        return (
          <a
            href={href}
            title={title}
            onClick={handleClick}
            target="_blank"
            rel="noopener noreferrer"
            {...aProps}
          >
            {aChildren}
          </a>
        );
      },
      img: ({
        src,
        alt,
        ...imgProps
      }: React.ImgHTMLAttributes<HTMLImageElement>) => config?.staticRendering && isSafeInlineRasterUrl(src) ? (
        <img {...imgProps} src={src} alt={alt ?? ""} />
      ) : <MarkdownImage src={src} alt={alt} config={config} {...imgProps} />,
    }),
    [config, complete, theme],
  );

  const themeClass =
    markdownTheme === "loose" || markdownTheme === "reasoning"
      ? "markdown-body md-font-size-fixed"
      : "markdown-body";

  return (
    <div className={themeClass} data-md-theme={markdownTheme}>
      <MarkdownErrorBoundary
        fallbackText={preprocessed}
        remarkPlugins={remarkPlugins as unknown[]}
        rehypePlugins={rehypePlugins as unknown[]}
        components={components as Record<string, unknown>}
        urlTransform={urlTransform}
      >
        <ReactMarkdown
          remarkPlugins={remarkPlugins as never}
          rehypePlugins={rehypePlugins as never}
          components={components as never}
          urlTransform={urlTransform}
        >
          {preprocessed}
        </ReactMarkdown>
      </MarkdownErrorBoundary>
    </div>
  );
}

/**
 * memo 自定义比较器必须覆盖所有进入 `rehypePlugins` / `components` /
 * `preprocessMarkdown` 等 `useMemo` 依赖的 prop。
 * 漏写任何一项,React 会跳过重渲染,导致下游依赖该 prop 的流水线(如
 * 会话内查找 findQuery)永远不刷新 — 这是历史 bug「搜索『鸟』返回 0/0」
 * 的根因。新增影响渲染的 prop 时,必须同步加入此处。
 */
export const Markdown = memo(MarkdownInner, (prev, next) => {
  return (
    prev.children === next.children &&
    prev.complete === next.complete &&
    prev.theme === next.theme &&
    prev.config === next.config &&
    prev.markdownTheme === next.markdownTheme &&
    prev.findQuery === next.findQuery
  );
});

export type { MarkdownProps, MarkdownConfig, MarkdownTheme } from "./types";
