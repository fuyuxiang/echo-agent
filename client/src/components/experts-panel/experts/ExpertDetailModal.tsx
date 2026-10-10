/**
 * Expert detail modal — shown when clicking an expert card.
 * Mirrors EchoAgent's TeamDetailModal: avatar, name, profession, category,
 * usage info, ability intro, skill tags, "试试这样问我" quick prompts,
 * and a "召唤 XXX" button at the bottom.
 */
import type { ExpertItem } from "@/lib/types";
import { ThumbImg } from "../shared/ThumbImg";
import { useModalFocus } from "@/lib/use-modal-focus";

interface Props {
  expert: ExpertItem;
  onClose: () => void;
  /** Called with (expert, promptOverride?) when user clicks 召唤 or a quickPrompt. */
  onSummon: (expert: ExpertItem, promptOverride?: string) => void;
}

export function ExpertDetailModal({ expert, onClose, onSummon }: Props) {
  const dialogRef = useModalFocus<HTMLDivElement>(true, onClose);

  const title = expert.title || expert.name;
  const sub = expert.name !== title ? expert.name : "";
  const quickPrompts = (expert.quickPrompts ?? []).filter(Boolean).slice(0, 5);

  return (
    <div
      className="ec-modal-overlay"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div ref={dialogRef} className="ec-modal ec-modal--managed" role="dialog" aria-modal="true" aria-label={`${title} 专家详情`} tabIndex={-1}>
        <button className="ec-modal-close" onClick={onClose} aria-label="关闭" data-modal-initial-focus>×</button>

        {/* Header: avatar + name + profession + category */}
        <div className="ec-modal-header">
          <ThumbImg
            name={expert.name}
            local={expert.avatarLocal}
            url={expert.avatarUrl}
            size={64}
            shape="square"
          />
          <div className="ec-modal-info">
            <div className="ec-modal-title">{title}</div>
            <div className="ec-modal-meta">
              {sub && <span>{sub}</span>}
              {sub && expert.cat && <span className="ec-modal-dot">·</span>}
              {expert.cat && <span>{expert.cat}</span>}
            </div>
          </div>
        </div>

        <div className="ec-modal-body">
        {/* Ability intro */}
        {expert.desc && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">能力介绍</div>
            <p className="ec-modal-desc">{expert.desc}</p>
          </div>
        )}

        {/* Tags */}
        {expert.tags.length > 0 && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">擅长领域</div>
            <div className="ec-modal-tags">
              {expert.tags.map((t, i) => (
                <span key={i} className="ec-modal-tag">{t}</span>
              ))}
            </div>
          </div>
        )}

        {/* Quick prompts */}
        {quickPrompts.length > 0 && (
          <div className="ec-modal-section">
            <div className="ec-modal-section-title">试试这样问我</div>
            <div className="ec-modal-quick-prompts">
              {quickPrompts.map((qp, i) => (
                <button
                  key={i}
                  className="ec-modal-qp-btn"
                  onClick={() => onSummon(expert, qp)}
                >
                  <span className="ec-modal-qp-text">"{qp}"</span>
                  <span className="ec-modal-qp-arrow">›</span>
                </button>
              ))}
            </div>
          </div>
        )}

        </div>
        <div className="ec-modal-footer">
        {/* Summon button */}
        <button
          className="ec-modal-summon-btn"
          onClick={() => onSummon(expert)}
        >
          召唤 {title}
        </button>
        </div>
      </div>
    </div>
  );
}
