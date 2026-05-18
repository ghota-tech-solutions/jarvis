/* @refresh reload */
import { render } from 'solid-js/web';
import { Router } from '@solidjs/router';
import { QueryClientProvider } from '@tanstack/solid-query';

import App from './App';
import { routes } from '~/lib/router';
import { queryClient } from '~/lib/query';

const root = document.getElementById('root');
if (!root) {
  throw new Error('Root element #root not found');
}

render(
  () => (
    <QueryClientProvider client={queryClient}>
      <Router root={App}>{routes}</Router>
    </QueryClientProvider>
  ),
  root
);
