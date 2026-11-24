import "./additional-automation-directories.css";

export function AdditionalAutomationDirectories({
  paths,
  disabled,
  onRemove,
}: {
  paths: readonly string[];
  disabled?: boolean;
  onRemove: (index: number) => void;
}) {
  if (!paths.length) return null;

  return (
    <div className="automation-extra-directories">
      <p>附加目录 · 任务在主工作目录运行</p>
      <ul aria-label="附加目录">
        {paths.map((path, index) => (
          <li key={`${index}:${path}`}>
            <span title={path}>{path}</span>
            <button type="button" className="btn btn--ghost" disabled={disabled}
              aria-label={`移除附加目录 ${path}`} onClick={() => onRemove(index)}>
              移除
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
