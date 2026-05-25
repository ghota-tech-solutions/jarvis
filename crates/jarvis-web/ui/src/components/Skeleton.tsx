import type { Component, JSX } from 'solid-js';
import { Index } from 'solid-js';

type SkeletonProps = {
  /** Width — CSS value (e.g. "100%", "200px", "12rem"). Default "100%". */
  width?: string;
  /** Height — CSS value. Default "1em". */
  height?: string;
  /** Border radius. Default "4px". */
  rounded?: string;
  /** Inline style override. */
  style?: JSX.CSSProperties;
};

export const Skeleton: Component<SkeletonProps> = (props) => {
  return (
    <span
      class="skeleton"
      aria-busy="true"
      aria-live="polite"
      style={{
        width: props.width ?? '100%',
        height: props.height ?? '1em',
        'border-radius': props.rounded ?? '4px',
        display: 'inline-block',
        ...props.style,
      }}
    />
  );
};

/** Composite: a typical card skeleton (header line + N-1 short lines). */
export const SkeletonCard: Component<{ lines?: number }> = (props) => {
  const lines = () => props.lines ?? 3;
  const bodyLines = () => Array.from({ length: Math.max(0, lines() - 1) });
  return (
    <div class="skeleton-card">
      <Skeleton height="1.1em" width="55%" />
      <Index each={bodyLines()}>
        {(_, i) => (
          <Skeleton
            height="0.85em"
            width={i === bodyLines().length - 1 ? '40%' : '90%'}
          />
        )}
      </Index>
    </div>
  );
};

/** Composite: a list of N cards (for Dashboard / Memory / Schedules). */
export const SkeletonList: Component<{ count?: number; lines?: number }> = (
  props,
) => {
  const count = () => props.count ?? 3;
  return (
    <div class="skeleton-list">
      <Index each={Array.from({ length: count() })}>
        {() => <SkeletonCard lines={props.lines} />}
      </Index>
    </div>
  );
};

export default Skeleton;
