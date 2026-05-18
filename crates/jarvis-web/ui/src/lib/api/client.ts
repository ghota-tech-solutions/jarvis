// Connect client for the daemon's gRPC-Web endpoint.
//
// Wraps `createGrpcWebTransport` with an interceptor that injects the bearer
// token from sessionStorage on every call. The token is bootstrapped in env.ts
// from the URL hash (#token=XXX) on first load.

import { createClient, type Interceptor } from '@connectrpc/connect';
import { createGrpcWebTransport } from '@connectrpc/connect-web';
import { API_BASE, getToken } from '~/lib/env';
import { Jarvis } from './gen/jarvis_pb';

const authInterceptor: Interceptor = (next) => async (req) => {
  const token = getToken();
  if (token) {
    req.header.set('Authorization', `Bearer ${token}`);
  }
  return next(req);
};

export const transport = createGrpcWebTransport({
  baseUrl: API_BASE,
  interceptors: [authInterceptor],
  // Browser fetch handles HTTP/1.1 → gRPC-Web framing.
});

export const jarvis = createClient(Jarvis, transport);

export type JarvisClient = typeof jarvis;
