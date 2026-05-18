import { QueryClient } from '@tanstack/solid-query';

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Most data is event-streamed via SSE/gRPC-Web; HTTP queries are
      // fallbacks. 5 minute stale time keeps the client gentle.
      staleTime: 5 * 60_000,
      gcTime: 30 * 60_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});
