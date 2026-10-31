/**
 * controls — 自动化面板的基础控件，1:1 复刻 EchoAgent automation-panel 的
 * DOM 结构与 class 命名（atm-tabs / atm-custom-select / atm-time-picker /
 * atm-validity-picker / atm-weekday-picker / atm-monthday-picker / echo-switch）。
 *
 * 所有下拉都通过 fixed 定位 + body portal 渲染（atm-floating-layer），
 * 避免被编辑页滚动容器裁剪。
 */
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { AlarmClockIcon, ChevronDownIcon } from "@/foundation/components/Icon/icons";
import {
  ALL_DAYS,
  DAY_LABELS,
  MONTH_DAYS,
  MONTHS,
  formatDateInputValue,
  pad2,
  sortMonthDays,
  sortWeekdays,
  type WeekdayCode,
} from "./schedule-utils";

// ============================================================
// Floating dropdown helpers
// ============================================================

const FLOATING_Z_INDEX = "var(--echo-layer-popover)";

interface FloatingOptions {
  preferredPlacement?: "top" | "bottom";
  offset?: number;
  width?: number | "anchor";
  maxWidth?: string;
  estimatedHeight?: number;
  horizontalMargin?: number;
}

function useFloatingDropdownStyle(
  triggerRef: React.RefObject<HTMLElement | null>,
  isOpen: boolean,
  options: FloatingOptions = {},
): CSSProperties | undefined {
  const {
    preferredPlacement = "top",
    offset = 6,
    width = "anchor",
    maxWidth,
    estimatedHeight = 240,
    horizontalMargin = 8,
  } = options;
  const [style, setStyle] = useState<CSSProperties | undefined>(undefined);

  useLayoutEffect(() => {
    if (!isOpen) {
      setStyle(undefined);
      return;
    }
    const update = () => {
      const trigger = triggerRef.current;
      if (!trigger) return;
      const rect = trigger.getBoundingClientRect();
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      const floatingWidth = width === "anchor" ? rect.width : width;
      const spaceAbove = rect.top - horizontalMargin - offset;
      const spaceBelow = vh - rect.bottom - horizontalMargin - offset;
      let placement = preferredPlacement;
      if (preferredPlacement === "top" && spaceAbove < Math.min(estimatedHeight, 160) && spaceBelow > spaceAbove) {
        placement = "bottom";
      } else if (preferredPlacement === "bottom" && spaceBelow < Math.min(estimatedHeight, 160) && spaceAbove > spaceBelow) {
        placement = "top";
      }
      const maxLeft = Math.max(horizontalMargin, vw - floatingWidth - horizontalMargin);
      const next: CSSProperties = {
        position: "fixed",
        left: Math.min(Math.max(horizontalMargin, rect.left), maxLeft),
        zIndex: FLOATING_Z_INDEX,
        maxWidth: maxWidth ?? `calc(100vw - ${horizontalMargin * 2}px)`,
        width: floatingWidth,
      };
      if (placement === "top") {
        next.bottom = Math.max(horizontalMargin, vh - rect.top + offset);
      } else {
        next.top = Math.min(vh - horizontalMargin, rect.bottom + offset);
      }
      setStyle(next);
    };
    update();
    const frame = window.requestAnimationFrame(update);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [isOpen, triggerRef, preferredPlacement, offset, width, maxWidth, estimatedHeight, horizontalMargin]);

  return style;
}

function FloatingLayer({ children }: { children: ReactNode }) {
  if (typeof document === "undefined") return <>{children}</>;
  return createPortal(<div className="atm-floating-layer">{children}</div>, document.body);
}

/** Close a dropdown when clicking outside both the root and the portalled dropdown. */
function useClickOutside(
  isOpen: boolean,
  refs: React.RefObject<HTMLElement | null>[],
  onClose: () => void,
) {
  useEffect(() => {
    if (!isOpen) return;
    const handler = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (refs.some((ref) => ref.current && ref.current.contains(target))) return;
      onClose();
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [isOpen, refs, onClose]);
}

// ============================================================
// Segmented — 药丸分段控件（定时任务/运行记录、周期/按间隔/单次）
// ============================================================

export interface SegmentedOption {
  value: string;
  label: ReactNode;
}

export function Segmented({
  value,
  options,
  onChange,
  className = "",
}: {
  value: string;
  options: SegmentedOption[];
  onChange: (value: string) => void;
  className?: string;
}) {
  return (
    <div className={`atm-segmented ${className}`} role="tablist">
      {options.map((opt) => (
        <button
          key={opt.value}
          type="button"
          role="tab"
          aria-selected={value === opt.value}
          className={`atm-segmented-item${value === opt.value ? " atm-segmented-item--active" : ""}`}
          onClick={() => onChange(opt.value)}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

// ============================================================
// Switch — 开关（推送到已配置通知渠道）
// ============================================================

export function Switch({
  checked,
  onChange,
  disabled,
  className = "",
  ariaLabel,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  className?: string;
  ariaLabel?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-label={ariaLabel}
      aria-checked={checked}
      disabled={disabled}
      className={`echo-switch${checked ? " echo-switch--checked" : ""}${disabled ? " echo-switch--disabled" : ""} ${className}`}
      onClick={() => onChange(!checked)}
    >
      <span className="echo-switch-thumb" />
    </button>
  );
}

// ============================================================
// Checkbox — 批量管理 / 权限确认弹窗
// ============================================================

export function Checkbox({
  checked,
  onChange,
  label,
  disabled,
  className = "",
}: {
  checked: boolean;
  onChange?: (checked: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <label className={`echo-checkbox${checked ? " echo-checkbox--checked" : ""}${disabled ? " echo-checkbox--disabled" : ""} ${className}`}>
      <input
        type="checkbox"
        className="echo-checkbox-input"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange?.(e.target.checked)}
      />
      <span className="echo-checkbox-box">
        {checked && (
          <svg width="10" height="10" viewBox="0 0 12 12" fill="none">
            <path d="M2 6.5L4.8 9L10 3.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        )}
      </span>
      {label != null && <span className="echo-checkbox-label">{label}</span>}
    </label>
  );
}

// ============================================================
// CustomSelect — 周期选择（每天/每周/双周/每月/每年）等
// ============================================================

export interface CustomSelectOption {
  label: string;
  value: string;
}

export function CustomSelect({
  value,
  options,
  placeholder,
  disabled,
  onChange,
}: {
  value: string;
  options: CustomSelectOption[];
  placeholder?: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const selected = useMemo(() => options.find((o) => o.value === value), [options, value]);
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: "anchor",
    estimatedHeight: 220,
  });
  useClickOutside(isOpen, [rootRef, dropdownRef], useCallback(() => setIsOpen(false), []));

  return (
    <div className={`atm-custom-select${isOpen ? " open" : ""}${disabled ? " disabled" : ""}`} ref={rootRef}>
      <button
        type="button"
        className={`atm-custom-select-trigger${isOpen ? " open" : ""} ${selected ? "has-value" : "placeholder"}`}
        disabled={disabled}
        onClick={() => setIsOpen((v) => !v)}
      >
        <span className="atm-custom-select-trigger-text">{selected?.label || placeholder || ""}</span>
        <ChevronDownIcon className="atm-custom-select-trigger-arrow" width={14} height={14} />
      </button>
      {isOpen && (
        <FloatingLayer>
          <div className="atm-custom-select-dropdown" ref={dropdownRef} style={dropdownStyle}>
            {options.map((opt) => (
              <button
                key={opt.value}
                type="button"
                className={`atm-custom-select-option${opt.value === value ? " active" : ""}`}
                onClick={() => {
                  onChange(opt.value);
                  setIsOpen(false);
                }}
              >
                {opt.label}
              </button>
            ))}
          </div>
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// TimePicker — 时间选择（值 + 闹钟图标，时/分两列下拉）
// ============================================================

const TIME_HOURS = Array.from({ length: 24 }, (_, i) => i);
const TIME_MINUTES = Array.from({ length: 60 }, (_, i) => i);

export function TimePicker({
  value,
  disabled,
  onChange,
}: {
  value: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const hourListRef = useRef<HTMLDivElement>(null);
  const minuteListRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const normalized = /^\d{1,2}:\d{2}$/.test(value) ? value : "09:00";
  const [hour, minute] = normalized.split(":").map(Number);
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: 220,
    estimatedHeight: 280,
  });
  useClickOutside(isOpen, [rootRef, dropdownRef], useCallback(() => setIsOpen(false), []));

  useEffect(() => {
    if (!isOpen) return;
    hourListRef.current?.querySelector(".active")?.scrollIntoView({ block: "center" });
    minuteListRef.current?.querySelector(".active")?.scrollIntoView({ block: "center" });
  }, [isOpen, hour, minute]);

  return (
    <div className={`atm-time-picker${isOpen ? " open" : ""}${disabled ? " disabled" : ""}`} ref={rootRef}>
      <button
        type="button"
        className="atm-time-picker-trigger"
        disabled={disabled}
        onClick={() => setIsOpen((v) => !v)}
      >
        <span className="atm-time-picker-value">{normalized}</span>
        <AlarmClockIcon width={14} height={14} className="atm-time-picker-icon" />
      </button>
      {isOpen && (
        <FloatingLayer>
          <div className="atm-time-picker-dropdown" ref={dropdownRef} style={dropdownStyle}>
            <div className="atm-time-picker-columns">
              <div className="atm-time-picker-column">
                <div className="atm-time-picker-column-title">时</div>
                <div className="atm-time-picker-options" ref={hourListRef}>
                  {TIME_HOURS.map((h) => (
                    <button
                      key={h}
                      type="button"
                      className={`atm-time-picker-option${h === hour ? " active" : ""}`}
                      onClick={() => onChange(`${pad2(h)}:${pad2(minute)}`)}
                    >
                      {pad2(h)}
                    </button>
                  ))}
                </div>
              </div>
              <div className="atm-time-picker-column">
                <div className="atm-time-picker-column-title">分</div>
                <div className="atm-time-picker-options" ref={minuteListRef}>
                  {TIME_MINUTES.map((m) => (
                    <button
                      key={m}
                      type="button"
                      className={`atm-time-picker-option${m === minute ? " active" : ""}`}
                      onClick={() => {
                        onChange(`${pad2(hour)}:${pad2(m)}`);
                        setIsOpen(false);
                      }}
                    >
                      {pad2(m)}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </div>
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// Calendar primitives（SingleDatePicker / ValidityRangePicker 共用）
// ============================================================

const WEEKDAY_HEADER = ["一", "二", "三", "四", "五", "六", "日"];

function parseDateInput(value?: string): Date | null {
  if (!value) return null;
  const d = new Date(`${value}T00:00:00`);
  return Number.isNaN(d.getTime()) ? null : d;
}

function compareDateValues(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function shiftMonth(view: Date, months: number): Date {
  return new Date(view.getFullYear(), view.getMonth() + months, 1);
}

interface CalendarCell {
  dateValue: string;
  dayNumber: number;
  isCurrentMonth: boolean;
  isToday: boolean;
  isRangeStart: boolean;
  isRangeEnd: boolean;
  isInRange: boolean;
  isPreviewStart: boolean;
  isPreviewEnd: boolean;
  isPreviewInRange: boolean;
}

function buildCalendarCells(
  viewMonth: Date,
  startDate?: string,
  endDate?: string,
  hoverDateValue?: string,
): CalendarCell[] {
  const firstWeekdayOffset = (new Date(viewMonth.getFullYear(), viewMonth.getMonth(), 1).getDay() + 6) % 7;
  const gridStart = new Date(viewMonth.getFullYear(), viewMonth.getMonth(), 1 - firstWeekdayOffset);
  const todayValue = formatDateInputValue(new Date());
  const hasPreview = !!startDate && !endDate && !!hoverDateValue;
  const previewStart = hasPreview
    ? compareDateValues(hoverDateValue!, startDate!) < 0
      ? hoverDateValue
      : startDate
    : undefined;
  const previewEnd = hasPreview
    ? compareDateValues(hoverDateValue!, startDate!) < 0
      ? startDate
      : hoverDateValue
    : undefined;
  return Array.from({ length: 42 }, (_, index) => {
    const date = new Date(gridStart.getFullYear(), gridStart.getMonth(), gridStart.getDate() + index);
    const dateValue = formatDateInputValue(date);
    return {
      dateValue,
      dayNumber: date.getDate(),
      isCurrentMonth: date.getMonth() === viewMonth.getMonth(),
      isToday: dateValue === todayValue,
      isRangeStart: !!startDate && dateValue === startDate,
      isRangeEnd: !!endDate && dateValue === endDate,
      isInRange:
        !!startDate && !!endDate &&
        compareDateValues(startDate, dateValue) < 0 &&
        compareDateValues(dateValue, endDate) < 0,
      isPreviewStart: !!previewStart && dateValue === previewStart,
      isPreviewEnd: !!previewEnd && dateValue === previewEnd,
      isPreviewInRange:
        !!previewStart && !!previewEnd &&
        compareDateValues(previewStart, dateValue) < 0 &&
        compareDateValues(dateValue, previewEnd) < 0,
    };
  });
}

function CalendarDropdown({
  viewMonth,
  setViewMonth,
  cells,
  todayValue,
  disablePast,
  onSelect,
  onHoverCell,
  onLeaveDays,
  hintText,
  onToday,
  dropdownRef,
  style,
}: {
  viewMonth: Date;
  setViewMonth: React.Dispatch<React.SetStateAction<Date>>;
  cells: CalendarCell[];
  todayValue: string;
  disablePast: boolean;
  onSelect: (dateValue: string) => void;
  onHoverCell?: (dateValue: string) => void;
  onLeaveDays?: () => void;
  hintText: string;
  onToday: () => void;
  dropdownRef: React.RefObject<HTMLDivElement>;
  style?: CSSProperties;
}) {
  const title = viewMonth.toLocaleDateString("zh-CN", { year: "numeric", month: "long" });
  return (
    <div className="atm-validity-dropdown" ref={dropdownRef} style={style}>
      <div className="atm-validity-calendar-header">
        <div className="atm-validity-nav-group">
          <button type="button" className="atm-validity-nav-btn" onClick={() => setViewMonth((v) => shiftMonth(v, -12))}>«</button>
          <button type="button" className="atm-validity-nav-btn" onClick={() => setViewMonth((v) => shiftMonth(v, -1))}>‹</button>
        </div>
        <span className="atm-validity-title">{title}</span>
        <div className="atm-validity-nav-group">
          <button type="button" className="atm-validity-nav-btn" onClick={() => setViewMonth((v) => shiftMonth(v, 1))}>›</button>
          <button type="button" className="atm-validity-nav-btn" onClick={() => setViewMonth((v) => shiftMonth(v, 12))}>»</button>
        </div>
      </div>
      <div className="atm-validity-weekdays">
        {WEEKDAY_HEADER.map((label, i) => (
          <span className="atm-validity-weekday" key={`${label}-${i}`}>{label}</span>
        ))}
      </div>
      <div className="atm-validity-days" onMouseLeave={onLeaveDays}>
        {cells.map((cell) => {
          const isDisabledDate = disablePast && compareDateValues(cell.dateValue, todayValue) < 0;
          return (
            <button
              type="button"
              key={cell.dateValue}
              className={[
                "atm-validity-day",
                cell.isCurrentMonth ? "" : "other-month",
                cell.isToday ? "today" : "",
                cell.isRangeStart ? "range-start" : "",
                cell.isRangeEnd ? "range-end" : "",
                cell.isInRange ? "in-range" : "",
                cell.isPreviewStart ? "preview-start" : "",
                cell.isPreviewEnd ? "preview-end" : "",
                cell.isPreviewInRange ? "preview-in-range" : "",
              ].filter(Boolean).join(" ")}
              disabled={isDisabledDate}
              onClick={() => {
                if (isDisabledDate) return;
                onSelect(cell.dateValue);
              }}
              onMouseEnter={() => {
                if (!isDisabledDate) onHoverCell?.(cell.dateValue);
              }}
            >
              {cell.dayNumber}
            </button>
          );
        })}
      </div>
      <div className="atm-validity-calendar-footer">
        <span className="atm-validity-hint-text">{hintText}</span>
        <button type="button" className="atm-validity-today-btn" onClick={onToday}>今天</button>
      </div>
    </div>
  );
}

// ============================================================
// SingleDatePicker — 单次执行日期（禁用过去日期）
// ============================================================

export function SingleDatePicker({
  value,
  disabled,
  onChange,
}: {
  value?: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [viewMonth, setViewMonth] = useState<Date>(() => {
    const base = parseDateInput(value) || new Date();
    return new Date(base.getFullYear(), base.getMonth(), 1);
  });
  const todayValue = formatDateInputValue(new Date());
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: 340,
    maxWidth: "min(340px, calc(100vw - 64px))",
    estimatedHeight: 360,
    horizontalMargin: 32,
  });
  useClickOutside(isOpen, [rootRef, dropdownRef], useCallback(() => setIsOpen(false), []));

  const cells = useMemo(
    () => buildCalendarCells(viewMonth, value, value, undefined),
    [viewMonth, value],
  );
  const displayText = value || "选择日期";

  const openPicker = () => {
    if (disabled) return;
    const base = parseDateInput(value) || new Date();
    setViewMonth(new Date(base.getFullYear(), base.getMonth(), 1));
    setIsOpen(true);
  };

  return (
    <div className="atm-validity-picker atm-single-date-picker" ref={rootRef}>
      <div
        className={`atm-validity-trigger${isOpen ? " active" : ""}${disabled ? " disabled" : ""}`}
        role="button"
        tabIndex={disabled ? -1 : 0}
        onClick={() => (isOpen ? setIsOpen(false) : openPicker())}
        onKeyDown={(e) => {
          if (disabled) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            isOpen ? setIsOpen(false) : openPicker();
          }
          if (e.key === "Escape") setIsOpen(false);
        }}
      >
        <span className={value ? "atm-validity-value" : "atm-validity-placeholder"}>{displayText}</span>
      </div>
      {isOpen && (
        <FloatingLayer>
          <CalendarDropdown
            viewMonth={viewMonth}
            setViewMonth={setViewMonth}
            cells={cells}
            todayValue={todayValue}
            disablePast
            onSelect={(dateValue) => {
              onChange(dateValue);
              setIsOpen(false);
            }}
            hintText={value || "点击选择日期"}
            onToday={() => {
              const today = new Date();
              setViewMonth(new Date(today.getFullYear(), today.getMonth(), 1));
              onChange(formatDateInputValue(today));
              setIsOpen(false);
            }}
            dropdownRef={dropdownRef}
            style={dropdownStyle}
          />
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// ValidityRangePicker — 生效日期区间（起始 + 结束，hover 预览）
// ============================================================

export function ValidityRangePicker({
  startDate,
  endDate,
  disabled,
  onChange,
}: {
  startDate?: string;
  endDate?: string;
  disabled?: boolean;
  onChange: (start: string, end: string) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [hoverDateValue, setHoverDateValue] = useState<string | undefined>(undefined);
  const [viewMonth, setViewMonth] = useState<Date>(() => {
    const base = parseDateInput(endDate || startDate) || new Date();
    return new Date(base.getFullYear(), base.getMonth(), 1);
  });
  const hasValue = !!startDate || !!endDate;
  const todayValue = formatDateInputValue(new Date());
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: 340,
    maxWidth: "min(340px, calc(100vw - 64px))",
    estimatedHeight: 360,
    horizontalMargin: 32,
  });
  useClickOutside(
    isOpen,
    [rootRef, dropdownRef],
    useCallback(() => {
      setIsOpen(false);
      setHoverDateValue(undefined);
    }, []),
  );

  const cells = useMemo(
    () => buildCalendarCells(viewMonth, startDate, endDate, hoverDateValue),
    [viewMonth, startDate, endDate, hoverDateValue],
  );

  const displayText = useMemo(() => {
    if (startDate && endDate) return `${startDate} — ${endDate}`;
    if (startDate) return `${startDate} —`;
    return "选择生效日期";
  }, [startDate, endDate]);

  const hintText = useMemo(() => {
    if (!startDate) return "点击选择起始日期";
    if (!endDate) return "点击选择结束日期";
    return `${startDate} — ${endDate}`;
  }, [startDate, endDate]);

  const openPicker = () => {
    if (disabled) return;
    const base = parseDateInput(endDate || startDate) || new Date();
    setViewMonth(new Date(base.getFullYear(), base.getMonth(), 1));
    setIsOpen(true);
  };

  const closePicker = () => {
    setIsOpen(false);
    setHoverDateValue(undefined);
  };

  const handleSelectDate = (dateValue: string) => {
    if (compareDateValues(dateValue, todayValue) < 0) return;
    if (!startDate || endDate) {
      onChange(dateValue, "");
      setHoverDateValue(undefined);
      return;
    }
    if (compareDateValues(dateValue, startDate) < 0) onChange(dateValue, startDate);
    else onChange(startDate, dateValue);
    setHoverDateValue(undefined);
    setIsOpen(false);
  };

  return (
    <div className="atm-validity-picker" ref={rootRef}>
      <div
        className={`atm-validity-trigger${isOpen ? " active" : ""}${hasValue ? " has-value" : ""}${disabled ? " disabled" : ""}`}
        role="button"
        tabIndex={disabled ? -1 : 0}
        onClick={() => (isOpen ? closePicker() : openPicker())}
        onKeyDown={(e) => {
          if (disabled) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            isOpen ? closePicker() : openPicker();
          }
          if (e.key === "Escape") closePicker();
        }}
      >
        <span className={hasValue ? "atm-validity-value" : "atm-validity-placeholder"}>{displayText}</span>
        {hasValue && (
          <div className="atm-validity-actions">
            <button
              type="button"
              className="atm-validity-clear"
              title="清除"
              disabled={disabled}
              onClick={(e) => {
                e.stopPropagation();
                onChange("", "");
                setHoverDateValue(undefined);
              }}
            >
              ×
            </button>
          </div>
        )}
      </div>
      {isOpen && (
        <FloatingLayer>
          <CalendarDropdown
            viewMonth={viewMonth}
            setViewMonth={setViewMonth}
            cells={cells}
            todayValue={todayValue}
            disablePast
            onSelect={handleSelectDate}
            onHoverCell={(dateValue) => {
              if (startDate && !endDate) setHoverDateValue(dateValue);
            }}
            onLeaveDays={() => setHoverDateValue(undefined)}
            hintText={hintText}
            onToday={() => {
              const today = new Date();
              setViewMonth(new Date(today.getFullYear(), today.getMonth(), 1));
              handleSelectDate(formatDateInputValue(today));
            }}
            dropdownRef={dropdownRef}
            style={dropdownStyle}
          />
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// WeekdayMultiPicker — 星期多选（标签 + 7 天网格）
// ============================================================

export function WeekdayMultiPicker({
  values,
  disabled,
  requireOne,
  onChange,
}: {
  values: string[];
  disabled?: boolean;
  requireOne?: boolean;
  onChange: (values: string[]) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const selectedDays = useMemo(() => sortWeekdays(values), [values]);
  const hasValue = selectedDays.length > 0;
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: 220,
    estimatedHeight: 180,
  });
  useClickOutside(isOpen, [rootRef, dropdownRef], useCallback(() => setIsOpen(false), []));

  const handleToggleDay = (day: WeekdayCode) => {
    const exists = selectedDays.includes(day);
    if (exists && requireOne && selectedDays.length === 1) return;
    onChange(exists ? selectedDays.filter((d) => d !== day) : sortWeekdays([...selectedDays, day]));
  };

  return (
    <div className={`atm-weekday-picker${isOpen ? " open dropdown-open" : ""}${disabled ? " disabled" : ""}`} ref={rootRef}>
      <div
        className={`atm-weekday-picker-trigger${isOpen ? " active" : ""}${disabled ? " disabled" : ""}`}
        role="button"
        tabIndex={disabled ? -1 : 0}
        onClick={() => !disabled && setIsOpen((v) => !v)}
        onKeyDown={(e) => {
          if (disabled) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setIsOpen((v) => !v);
          }
          if (e.key === "Escape") setIsOpen(false);
        }}
      >
        {hasValue ? (
          <div className="atm-weekday-picker-tags">
            {selectedDays.map((day) => (
              <span className="atm-weekday-picker-tag" key={day}>
                {DAY_LABELS[day]}
                {!(requireOne && selectedDays.length === 1) && (
                  <button
                    type="button"
                    className="atm-weekday-picker-tag-remove"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleToggleDay(day);
                    }}
                  >
                    ×
                  </button>
                )}
              </span>
            ))}
          </div>
        ) : (
          <span className="atm-weekday-picker-placeholder">选择星期</span>
        )}
      </div>
      {hasValue && !requireOne && (
        <button
          type="button"
          className="atm-weekday-picker-clear"
          title="清除"
          disabled={disabled}
          onClick={(e) => {
            e.stopPropagation();
            onChange([]);
          }}
        >
          ×
        </button>
      )}
      {isOpen && (
        <FloatingLayer>
          <div className="atm-weekday-picker-dropdown" ref={dropdownRef} style={dropdownStyle}>
            <div className="atm-weekday-grid">
              {ALL_DAYS.map((day) => (
                <button
                  key={day}
                  type="button"
                  className={`atm-weekday-item${selectedDays.includes(day) ? " active" : ""}`}
                  onClick={() => handleToggleDay(day)}
                >
                  {DAY_LABELS[day]}
                </button>
              ))}
            </div>
          </div>
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// MonthdayMultiPicker — 每月日期多选（标签 + 31 天网格）
// ============================================================

export function MonthdayMultiPicker({
  values,
  disabled,
  onChange,
}: {
  values: number[];
  disabled?: boolean;
  onChange: (values: number[]) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const selectedDays = useMemo(() => sortMonthDays(values), [values]);
  const hasValue = selectedDays.length > 0;
  const dropdownStyle = useFloatingDropdownStyle(rootRef, isOpen, {
    preferredPlacement: "bottom",
    width: 220,
    estimatedHeight: 180,
  });
  useClickOutside(isOpen, [rootRef, dropdownRef], useCallback(() => setIsOpen(false), []));

  const handleToggleDay = (day: number) => {
    onChange(
      selectedDays.includes(day)
        ? selectedDays.filter((d) => d !== day)
        : sortMonthDays([...selectedDays, day]),
    );
  };

  return (
    <div className={`atm-monthday-picker${isOpen ? " open dropdown-open" : ""}${disabled ? " disabled" : ""}`} ref={rootRef}>
      <div
        className={`atm-monthday-picker-trigger${isOpen ? " active" : ""}${disabled ? " disabled" : ""}`}
        role="button"
        tabIndex={disabled ? -1 : 0}
        onClick={() => !disabled && setIsOpen((v) => !v)}
        onKeyDown={(e) => {
          if (disabled) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            setIsOpen((v) => !v);
          }
          if (e.key === "Escape") setIsOpen(false);
        }}
      >
        {hasValue ? (
          <div className="atm-monthday-picker-tags">
            {selectedDays.map((day) => (
              <span className="atm-monthday-picker-tag" key={day}>
                {day}日
                <button
                  type="button"
                  className="atm-monthday-picker-tag-remove"
                  onClick={(e) => {
                    e.stopPropagation();
                    handleToggleDay(day);
                  }}
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        ) : (
          <span className="atm-monthday-picker-placeholder">选择日期</span>
        )}
      </div>
      {hasValue && (
        <button
          type="button"
          className="atm-monthday-picker-clear"
          title="清除"
          disabled={disabled}
          onClick={(e) => {
            e.stopPropagation();
            onChange([]);
          }}
        >
          ×
        </button>
      )}
      {isOpen && (
        <FloatingLayer>
          <div className="atm-monthday-picker-dropdown" ref={dropdownRef} style={dropdownStyle}>
            <div className="atm-monthday-grid">
              {MONTH_DAYS.map((day) => (
                <button
                  key={day}
                  type="button"
                  className={`atm-monthday-item${selectedDays.includes(day) ? " active" : ""}`}
                  onClick={() => handleToggleDay(day)}
                >
                  {day}日
                </button>
              ))}
            </div>
          </div>
        </FloatingLayer>
      )}
    </div>
  );
}

// ============================================================
// IntervalDayChips — 按间隔模式下的星期点选（一二三四五六日）
// ============================================================

export function IntervalDayChips({
  values,
  disabled,
  onToggle,
}: {
  values: string[];
  disabled?: boolean;
  onToggle: (day: WeekdayCode) => void;
}) {
  return (
    <div className="atm-schedule-days">
      {ALL_DAYS.map((day) => (
        <div
          key={day}
          role="button"
          tabIndex={disabled ? -1 : 0}
          aria-pressed={values.includes(day)}
          aria-disabled={disabled}
          className={`atm-schedule-day${values.includes(day) ? " active" : ""}${disabled ? " disabled" : ""}`}
          onClick={() => {
            if (!disabled) onToggle(day);
          }}
          onKeyDown={(e) => {
            if (disabled) return;
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onToggle(day);
            }
          }}
        >
          {DAY_LABELS[day]}
        </div>
      ))}
    </div>
  );
}

export { MONTHS };
