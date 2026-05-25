// /settings — Centralized user preferences page.
//
// All controls write through the persistent atoms in `~/lib/settings.ts`,
// which are bound to <html> via `useAppearance()` in App. No commit button:
// edits apply live.

import { For, Show, type Component } from 'solid-js';
import { useStore } from '@nanostores/solid';
import { getToken } from '~/lib/env';
import { API_BASE } from '~/lib/env';
import {
  $theme,
  $accent,
  $density,
  $defaultSandbox,
  $defaultRouting,
  $defaultMaxSteps,
  $notificationsEnabled,
  ACCENT_PRESETS,
  type ThemeMode,
  type Density,
  type DefaultSandbox,
  type DefaultRouting,
} from '~/lib/settings';
import { requestNotificationPermission } from '~/lib/notify';
import { $locale, SUPPORTED_LOCALES, useT, type Locale } from '~/lib/i18n';

// --- Generic helpers -----------------------------------------------------

type RadioOption<T extends string> = { value: T; label: string; hint?: string };

const RadioGroup = <T extends string>(p: {
  name: string;
  value: T;
  options: ReadonlyArray<RadioOption<T>>;
  onChange: (v: T) => void;
}) => (
  <div class="settings-radio-row" role="radiogroup" aria-label={p.name}>
    <For each={p.options}>
      {(opt) => (
        <label class={`settings-radio ${p.value === opt.value ? 'is-active' : ''}`}>
          <input
            type="radio"
            name={p.name}
            value={opt.value}
            checked={p.value === opt.value}
            onChange={() => p.onChange(opt.value)}
          />
          <span>{opt.label}</span>
          <Show when={opt.hint}>
            <span class="dim settings-radio-hint">{opt.hint}</span>
          </Show>
        </label>
      )}
    </For>
  </div>
);

const Section: Component<{ title: string; desc?: string; children: any }> = (p) => (
  <section class="settings-section">
    <header class="settings-section-head">
      <h3 class="heading settings-section-title">{p.title}</h3>
      <Show when={p.desc}>
        <p class="dim settings-section-desc">{p.desc}</p>
      </Show>
    </header>
    <div class="settings-section-body">{p.children}</div>
  </section>
);

const Row: Component<{ label: string; hint?: string; children: any }> = (p) => (
  <div class="settings-row">
    <div class="settings-row-label">
      <div>{p.label}</div>
      <Show when={p.hint}>
        <div class="dim settings-row-hint">{p.hint}</div>
      </Show>
    </div>
    <div class="settings-row-control">{p.children}</div>
  </div>
);

// --- Sections ------------------------------------------------------------

const AppearanceSection: Component = () => {
  const theme = useStore($theme);
  const accent = useStore($accent);
  const density = useStore($density);

  const themeOpts: ReadonlyArray<RadioOption<ThemeMode>> = [
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
    { value: 'system', label: 'System', hint: 'follow OS' },
  ];

  const densityOpts: ReadonlyArray<RadioOption<Density>> = [
    { value: 'comfortable', label: 'Comfortable' },
    { value: 'compact', label: 'Compact' },
  ];

  return (
    <Section title="Appearance" desc="Theme, accent color, and layout density.">
      <Row label="Theme">
        <RadioGroup
          name="theme"
          value={theme()}
          options={themeOpts}
          onChange={(v) => $theme.set(v)}
        />
      </Row>
      <Row label="Accent color" hint="Applied to highlights, links, and active states.">
        <div class="settings-swatches" role="radiogroup" aria-label="Accent color">
          <For each={ACCENT_PRESETS}>
            {(p) => (
              <button
                type="button"
                class={`settings-swatch ${accent() === p.value ? 'is-active' : ''}`}
                style={`--swatch: ${p.value}`}
                title={p.name}
                aria-label={`Accent ${p.name}`}
                aria-pressed={accent() === p.value}
                onClick={() => $accent.set(p.value)}
              />
            )}
          </For>
        </div>
      </Row>
      <Row label="Density" hint="Tighter paddings on compact (full effect lands in F2).">
        <RadioGroup
          name="density"
          value={density()}
          options={densityOpts}
          onChange={(v) => $density.set(v)}
        />
      </Row>
    </Section>
  );
};

const DefaultsSection: Component = () => {
  const sandbox = useStore($defaultSandbox);
  const routing = useStore($defaultRouting);
  const maxSteps = useStore($defaultMaxSteps);

  const sandboxOpts: ReadonlyArray<RadioOption<DefaultSandbox>> = [
    { value: 'native', label: 'native' },
    { value: 'docker', label: 'docker' },
    { value: 'wsl2', label: 'wsl2' },
  ];

  const routingOpts: ReadonlyArray<RadioOption<DefaultRouting>> = [
    { value: 'auto', label: 'auto' },
    { value: 'local_only', label: 'local only' },
    { value: 'remote_only', label: 'remote only' },
  ];

  return (
    <Section
      title="Defaults"
      desc="Pre-filled values for new tasks. Per-task overrides still win."
    >
      <Row label="Default sandbox">
        <RadioGroup
          name="default-sandbox"
          value={sandbox()}
          options={sandboxOpts}
          onChange={(v) => $defaultSandbox.set(v)}
        />
      </Row>
      <Row label="Default routing">
        <RadioGroup
          name="default-routing"
          value={routing()}
          options={routingOpts}
          onChange={(v) => $defaultRouting.set(v)}
        />
      </Row>
      <Row label="Default max steps" hint="Hard cap on agent iterations (0 = daemon default).">
        <input
          type="number"
          class="input"
          min="0"
          max="500"
          step="1"
          value={maxSteps()}
          onInput={(e) => {
            const n = Number(e.currentTarget.value);
            if (Number.isFinite(n) && n >= 0) $defaultMaxSteps.set(n);
          }}
          style="width: 7rem"
        />
      </Row>
    </Section>
  );
};

const NotificationsSection: Component = () => {
  const enabled = useStore($notificationsEnabled);

  const onToggle = async (next: boolean) => {
    $notificationsEnabled.set(next);
    if (next) {
      // Best-effort: re-ask permission when the user turns it back on,
      // so a previous "denied" state isn't silently honoured forever.
      await requestNotificationPermission();
    }
  };

  return (
    <Section
      title="Notifications"
      desc="OS-level pings when a task finishes. Uses the Tauri plugin on desktop, the browser Notification API on web."
    >
      <Row label="Enable task notifications">
        <label class="switch">
          <input
            type="checkbox"
            checked={enabled()}
            onChange={(e) => void onToggle(e.currentTarget.checked)}
          />
          <span>{enabled() ? 'on' : 'off'}</span>
        </label>
      </Row>
    </Section>
  );
};

// Source of truth for the hotkeys table is `HelpOverlay.tsx`. Keep this in
// sync with that file — F2 will lift hotkeys into their own store with
// rebind support, at which point this becomes editable.
const HOTKEYS: ReadonlyArray<{ keys: string; desc: string; scope: string }> = [
  { keys: '?',         desc: 'toggle help overlay',         scope: 'Global' },
  { keys: '/',         desc: 'focus dashboard search',      scope: 'Global' },
  { keys: 'g d',       desc: 'go to dashboard',             scope: 'Global' },
  { keys: 't',         desc: 'toggle theme dark/light',     scope: 'Global' },
  { keys: 'Esc',       desc: 'close overlays',              scope: 'Global' },
  { keys: 'Cmd/Ctrl+K',desc: 'command palette',             scope: 'Global' },
  { keys: '[ / ]',     desc: 'previous / next span',        scope: 'Timeline' },
  { keys: 'j / k',     desc: 'previous / next event',       scope: 'Timeline' },
  { keys: '1-4',       desc: 'toggle lanes',                scope: 'Timeline' },
  { keys: 'f',         desc: 'fit timeline to data',        scope: 'Timeline' },
  { keys: 'l',         desc: 'toggle follow-live',          scope: 'Timeline' },
];

const HotkeysSection: Component = () => (
  <Section
    title="Hotkeys"
    desc="Read-only. Customization coming in F2."
  >
    <table class="settings-hotkeys">
      <thead>
        <tr>
          <th>Keys</th>
          <th>Action</th>
          <th>Scope</th>
        </tr>
      </thead>
      <tbody>
        <For each={HOTKEYS}>
          {(h) => (
            <tr>
              <td><kbd>{h.keys}</kbd></td>
              <td>{h.desc}</td>
              <td class="dim">{h.scope}</td>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  </Section>
);

const AboutSection: Component = () => {
  const version = import.meta.env.VITE_APP_VERSION ?? '0.1.1';
  const tokenPresent = !!getToken();
  return (
    <Section title="About">
      <Row label="Version">
        <code>{version}</code>
      </Row>
      <Row label="Daemon URL">
        <code class="dim">{API_BASE}</code>
      </Row>
      <Row label="Web token">
        <Show
          when={tokenPresent}
          fallback={
            <span class="error">
              absent — append <code>#token=&lt;web.token&gt;</code> to the URL
            </span>
          }
        >
          <span class="good">present</span>
        </Show>
      </Row>
    </Section>
  );
};

const LanguageSection: Component = () => {
  const t = useT();
  const locale = useStore($locale);

  const opts: ReadonlyArray<RadioOption<Locale>> = SUPPORTED_LOCALES.map(
    (l) => ({ value: l.code, label: l.label }),
  );

  return (
    <Section title={t().settings.language} desc={t().settings.language_desc}>
      <Row label={t().settings.language}>
        <RadioGroup
          name="locale"
          value={locale()}
          options={opts}
          onChange={(v) => $locale.set(v)}
        />
      </Row>
    </Section>
  );
};

// --- Page ----------------------------------------------------------------

const Settings: Component = () => {
  const t = useT();
  return (
    <section class="settings-page">
      <header class="settings-header">
        <h2 class="heading" style="margin: 0">{t().settings.title}</h2>
        <p class="dim" style="margin: 0.2rem 0 0 0; font-size: 12px">
          {t().settings.desc}
        </p>
      </header>
      <LanguageSection />
      <AppearanceSection />
      <DefaultsSection />
      <NotificationsSection />
      <HotkeysSection />
      <AboutSection />
    </section>
  );
};

export default Settings;
