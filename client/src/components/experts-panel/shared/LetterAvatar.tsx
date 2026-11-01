import { avatarTextColor } from "../../../lib/avatar-color";
import { useState } from "react";

/** Stable 10-color palette (EchoAgent-style) used for letter fallbacks. */
const PALETTE = [
  "#6366f1", "#8b5cf6", "#ec4899", "#f97316", "#14b8a6",
  "#3b82f6", "#10b981", "#f43f5e", "#06b6d4", "#a855f7",
];

function hashColor(seed: string): string {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return PALETTE[h % PALETTE.length];
}

/** First "letter" of a name — works for CJK (first glyph) and latin. */
function initial(name: string): string {
  const t = (name || "").trim();
  if (!t) return "?";
  // Use the first code point so surrogate pairs / CJK glyphs stay intact.
  return Array.from(t)[0]!.toUpperCase();
}

interface LetterAvatarProps {
  /** Text used for the fallback glyph + color seed. */
  name: string;
  /** Optional remote/local image. Falls back to the letter on error. */
  src?: string;
  /** Pixel size (square). Default 40. */
  size?: number;
  /** Force a background color; otherwise derived from `name`. */
  color?: string;
  shape?: "circle" | "square";
  className?: string;
}

/** Rounded avatar: renders `src` when available, otherwise a colored letter
 *  tile (the same fallback EchoAgent uses when a COS avatar 404s). */
export function LetterAvatar({
  name, src, size = 40, color, shape = "circle", className,
}: LetterAvatarProps) {
  const [failed, setFailed] = useState(false);
  const bg = color || hashColor(name || "x");
  const showImg = !!src && !failed;
  const radius = shape === "circle" ? "50%" : Math.round(size * 0.28);

  const style: React.CSSProperties = {
    width: size,
    height: size,
    borderRadius: radius,
    background: showImg ? "var(--echo-bg-tertiary)" : bg,
    color: avatarTextColor(bg),
    fontSize: Math.round(size * 0.42),
    flex: "0 0 auto",
  };

  return (
    <span className={`um-avatar${className ? ` ${className}` : ""}`} style={style}
      aria-label={name} role="img">
      {showImg ? (
        <img src={src} alt="" loading="lazy" referrerPolicy="no-referrer"
          onError={() => setFailed(true)}
          style={{ width: "100%", height: "100%", objectFit: "cover", borderRadius: "inherit" }} />
      ) : (
        <span style={{ fontWeight: 600, lineHeight: 1 }}>{initial(name)}</span>
      )}
    </span>
  );
}
