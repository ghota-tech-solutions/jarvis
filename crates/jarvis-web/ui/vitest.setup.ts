// Vitest global setup — extends `expect` with @testing-library/jest-dom
// matchers (toBeInTheDocument, toHaveTextContent, etc.) and patches the
// DOM-ish globals happy-dom doesn't ship with.

import '@testing-library/jest-dom/vitest';

// happy-dom doesn't implement matchMedia. Some components (theme store)
// call it at module-eval time, so install a noop shim before any test
// module imports run.
if (!('matchMedia' in window)) {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}
