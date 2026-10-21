/**
 * Security policy surface (task T-059).
 *
 * The policy lives in `index.html` because a meta CSP has to exist before the
 * first module request. This module reads it back for tests and exposes the
 * header form that the deployment pipeline attaches in T-057, so there is one
 * policy value rather than a hand-maintained copy.
 */

export const CSP_META_NAME = "Content-Security-Policy";

export function contentSecurityPolicy(doc: Document = document): string {
  const meta = doc.querySelector<HTMLMetaElement>(
    `meta[http-equiv="${CSP_META_NAME}"]`,
  );
  return meta?.content ?? "";
}

/** Headers the fallback host must send; Vercel uses the same values. */
export function securityHeaders(doc: Document = document): Record<string, string> {
  return {
    "Content-Security-Policy": contentSecurityPolicy(doc),
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  };
}
