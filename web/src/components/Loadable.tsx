import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

/**
 * 统一渲染 useApi 的三态:loading / error / 空。请求失败不再静默卡在“加载中”,
 * 而是显示明确的错误。data 就绪后交给 children 渲染。
 *
 * loading 仅在“首次加载(还没有任何 data)”时展示,避免 refetch 时闪烁。
 */
export function Loadable<T>({
  loading,
  error,
  data,
  isEmpty,
  emptyText,
  children,
}: {
  loading: boolean;
  error: string | null;
  data: T | null;
  isEmpty?: (data: T) => boolean;
  emptyText?: string;
  children: (data: T) => ReactNode;
}) {
  const { t } = useTranslation("common");
  const empty = emptyText ?? t("noData");
  if (error) {
    return (
      <div className="bg-red-50 text-red-600 border border-red-200 rounded-lg p-4 text-sm">
        {t("loadFailed", { error })}
      </div>
    );
  }
  if (data === null) {
    if (loading) return <div className="text-gray-400 text-sm p-4">{t("loading")}</div>;
    return <div className="text-gray-400 text-sm p-4">{empty}</div>;
  }
  if (isEmpty && isEmpty(data)) {
    return <div className="text-gray-400 text-center py-8">{empty}</div>;
  }
  return <>{children(data)}</>;
}
