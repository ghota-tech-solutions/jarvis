import { Show, type Component } from 'solid-js';
import { useHelpOpen, setHelpOpenSig } from '~/lib/stores/shortcuts';

const shortcuts: Array<[string, string]> = [
  ['?', 'toggle this help'],
  ['/', 'focus dashboard search'],
  ['g d', 'go to dashboard'],
  ['t', 'toggle theme dark/light'],
  ['Esc', 'close overlays / clear selection'],
  ['', ''],
  ['Timeline:', ''],
  ['[ / ]', 'previous / next span'],
  ['j / k', 'previous / next event'],
  ['1-4', 'toggle lanes (tools/llm/plan/verdict)'],
  ['f', 'fit timeline to data'],
  ['l', 'toggle follow-live'],
  ['scroll', 'pan timeline'],
  ['ctrl+scroll', 'zoom at cursor'],
  ['drag handle', 'scrub playhead'],
];

const HelpOverlay: Component = () => {
  return (
    <Show when={useHelpOpen()}>
      <div
        class="help-backdrop"
        onClick={() => setHelpOpenSig(false)}
      >
        <div class="help-modal" onClick={(e) => e.stopPropagation()}>
          <h3 class="heading" style="margin-top: 0">Keyboard shortcuts</h3>
          <table>
            <tbody>
              {shortcuts.map(([k, v]) => (
                <tr>
                  <td>{k && <kbd>{k}</kbd>}</td>
                  <td class="dim">{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p class="dim" style="margin-bottom: 0; font-size: 11px">
            press <kbd>?</kbd> or click outside to close
          </p>
        </div>
      </div>
    </Show>
  );
};

export default HelpOverlay;
