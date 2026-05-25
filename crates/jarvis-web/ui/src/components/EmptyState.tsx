import type { Component, JSX } from 'solid-js';

type EmptyStateProps = {
  /** Optional inline SVG element (passed as JSX). Defaults to a minimalist
   *  empty-box outline. */
  illustration?: JSX.Element;
  /** Short headline (1 sentence). */
  title: string;
  /** Longer hint (1-2 sentences). */
  hint?: string;
  /** Optional action button label. */
  actionLabel?: string;
  /** Action click handler. */
  onAction?: () => void;
  /** Optional href if the action is a navigation (renders an <a> instead of button). */
  actionHref?: string;
};

const DefaultIllustration: Component = () => (
  <svg
    width="64"
    height="64"
    viewBox="0 0 64 64"
    fill="none"
    stroke="currentColor"
    stroke-width="1.5"
    aria-hidden="true"
  >
    <rect x="10" y="18" width="44" height="32" rx="3" />
    <path d="M10 26 L54 26" />
    <circle cx="20" cy="22" r="1.5" fill="currentColor" />
    <circle cx="26" cy="22" r="1.5" fill="currentColor" />
  </svg>
);

export const EmptyState: Component<EmptyStateProps> = (props) => {
  const renderAction = () => {
    if (!props.actionLabel) return null;
    if (props.actionHref) {
      return (
        <a href={props.actionHref} class="empty-state-action btn">
          {props.actionLabel}
        </a>
      );
    }
    return (
      <button
        type="button"
        class="empty-state-action btn"
        onClick={() => props.onAction?.()}
      >
        {props.actionLabel}
      </button>
    );
  };

  return (
    <div class="empty-state" role="status">
      <div class="empty-state-illustration">
        {props.illustration ?? <DefaultIllustration />}
      </div>
      <h3 class="empty-state-title">{props.title}</h3>
      {props.hint && <p class="empty-state-hint">{props.hint}</p>}
      {renderAction()}
    </div>
  );
};

export default EmptyState;
