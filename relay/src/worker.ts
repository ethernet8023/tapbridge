/// <reference types="@cloudflare/workers-types" />

export interface Env {
	/** Raw service-account JSON (contains private_key, client_email, project_id). */
	FCM_SERVICE_ACCOUNT: string;
	WAKE_LIMIT: RateLimit;
}

interface ServiceAccount {
	client_email: string;
	private_key: string;
	project_id: string;
}

interface CachedAccessToken {
	accessToken: string;
	/** Epoch seconds after which the token must be refreshed (expires ~5 min early). */
	expiresAtSec: number;
	projectId: string;
}

// Module-scope OAuth token cache. Keyed by the service account's client_email so a
// rotated secret invalidates the old entry.
const tokenCache = new Map<string, CachedAccessToken>();

const FCM_SCOPE = "https://www.googleapis.com/auth/firebase.messaging";
const OAUTH_TOKEN_URL = "https://oauth2.googleapis.com/token";
const MAX_BODY_BYTES = 8 * 1024;
const OAUTH_REFRESH_MARGIN_SEC = 300;
const TOKEN_HASH_LOG_PREFIX = 8; // hex chars of the SHA-256 token prefix we may log

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function b64urlEncode(bytes: Uint8Array): string {
	let bin = "";
	for (const b of bytes) bin += String.fromCharCode(b);
	return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function b64urlDecodeToBytes(s: string): Uint8Array {
	const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
	const pad = b64.length % 4 === 0 ? "" : "=".repeat(4 - (b64.length % 4));
	const bin = atob(b64 + pad);
	const out = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
	return out;
}

function utf8Bytes(s: string): Uint8Array {
	return new TextEncoder().encode(s);
}

async function sha256Hex(data: string): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-256", utf8Bytes(data));
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Import a PKCS#8 PEM private key for RS256 signing. */
function importSigningKey(pem: string): Promise<CryptoKey> {
	const body = pem
		.replace(/-----BEGIN PRIVATE KEY-----/, "")
		.replace(/-----END PRIVATE KEY-----/, "")
		.replace(/\s+/g, "");
	const raw = atob(body);
	const derBytes = new Uint8Array(raw.length);
	for (let i = 0; i < raw.length; i++) derBytes[i] = raw.charCodeAt(i);
	return crypto.subtle.importKey(
		"pkcs8",
		derBytes,
		{ name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
		false,
		["sign"],
	);
}

function jsonError(status: number, body: Record<string, unknown>): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { "content-type": "application/json" },
	});
}

/** Strict field validation: exact types, 1..=max length. */
function validateWake(body: unknown): { token: string; host: string; hint: string } | null {
	if (body === null || typeof body !== "object" || Array.isArray(body)) return null;
	const o = body as Record<string, unknown>;
	const { token, host, hint } = o;
	if (typeof token !== "string" || typeof host !== "string" || typeof hint !== "string") return null;
	if (token.length < 1 || token.length > 4096) return null;
	if (host.length < 1 || host.length > 128) return null;
	if (hint.length < 1 || hint.length > 1024) return null;
	return { token, host, hint };
}

function parseServiceAccount(json: string): ServiceAccount | null {
	let parsed: unknown;
	try {
		parsed = JSON.parse(json);
	} catch {
		return null;
	}
	if (parsed === null || typeof parsed !== "object") return null;
	const o = parsed as Record<string, unknown>;
	const { client_email, private_key, project_id } = o;
	if (
		typeof client_email !== "string" ||
		client_email.length === 0 ||
		typeof private_key !== "string" ||
		private_key.length === 0 ||
		typeof project_id !== "string" ||
		project_id.length === 0
	) {
		return null;
	}
	return { client_email, private_key, project_id };
}

// ---------------------------------------------------------------------------
// OAuth (JWT bearer -> access token), cached in module scope
// ---------------------------------------------------------------------------

async function getAccessToken(sa: ServiceAccount, fetchImpl: typeof fetch): Promise<{ accessToken: string; projectId: string } | null> {
	const cached = tokenCache.get(sa.client_email);
	const nowSec = Math.floor(Date.now() / 1000);
	if (cached && cached.expiresAtSec - OAUTH_REFRESH_MARGIN_SEC > nowSec && cached.projectId === sa.project_id) {
		return { accessToken: cached.accessToken, projectId: cached.projectId };
	}

	const iat = nowSec;
	const exp = iat + 3600; // exp <= 1h
	const header = b64urlEncode(utf8Bytes(JSON.stringify({ alg: "RS256", typ: "JWT" })));
	const claims = b64urlEncode(
		utf8Bytes(
			JSON.stringify({
				iss: sa.client_email,
				scope: FCM_SCOPE,
				aud: OAUTH_TOKEN_URL,
				exp,
				iat,
			}),
		),
	);
	const signingInput = `${header}.${claims}`;

	let signature: ArrayBuffer;
	try {
		const key = await importSigningKey(sa.private_key);
		signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, utf8Bytes(signingInput));
	} catch {
		return null; // malformed key in the secret
	}
	const assertion = `${signingInput}.${b64urlEncode(new Uint8Array(signature))}`;

	let resp: Response;
	try {
		resp = await fetchImpl(OAUTH_TOKEN_URL, {
			method: "POST",
			headers: { "content-type": "application/x-www-form-urlencoded" },
			body: new URLSearchParams({
				grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer",
				assertion,
			}).toString(),
		});
	} catch {
		return null;
	}
	if (resp.status !== 200) return null;
	let tokenJson: { access_token?: unknown; expires_in?: unknown };
	try {
		tokenJson = await resp.json();
	} catch {
		return null;
	}
	if (typeof tokenJson.access_token !== "string" || tokenJson.access_token.length === 0) return null;
	const expiresIn = typeof tokenJson.expires_in === "number" ? tokenJson.expires_in : 3600;
	const accessToken = tokenJson.access_token;
	tokenCache.set(sa.client_email, {
		accessToken,
		expiresAtSec: nowSec + Math.max(60, Math.min(expiresIn, 3600)),
		projectId: sa.project_id,
	});
	return { accessToken, projectId: sa.project_id };
}

// ---------------------------------------------------------------------------
// FCM send
// ---------------------------------------------------------------------------

interface FcmOutcome {
	kind: "accepted" | "unregistered" | "failure";
}

export async function sendFcmWake(
	projectId: string,
	accessToken: string,
	wake: { token: string; host: string; hint: string },
	fetchImpl: typeof fetch,
): Promise<FcmOutcome> {
	const url = `https://fcm.googleapis.com/v1/projects/${projectId}/messages:send`;
	const payload = {
		message: {
			token: wake.token,
			data: { t: "wake", host: wake.host, hint: wake.hint },
			android: { priority: "HIGH", ttl: "60s" },
		},
	};
	let resp: Response;
	try {
		resp = await fetchImpl(url, {
			method: "POST",
			headers: {
				authorization: `Bearer ${accessToken}`,
				"content-type": "application/json",
			},
			body: JSON.stringify(payload),
		});
	} catch {
		return { kind: "failure" };
	}
	if (resp.status >= 200 && resp.status < 300) return { kind: "accepted" };
	if (resp.status === 404) return { kind: "unregistered" };
	// FCM HTTP v1: 400 with INVALID_ARGUMENT on a bad token, 404 UNREGISTERED on a
	// stale one. Both mean the phone will never receive it.
	if (resp.status === 400) {
		try {
			const err = (await resp.json()) as { error?: { details?: Array<{ errorType?: string }> } };
			const details = err?.error?.details ?? [];
			if (details.some((d) => d?.errorType === "UNREGISTERED")) return { kind: "unregistered" };
		} catch {
			/* body not JSON: treat as invalid token anyway */
		}
		return { kind: "unregistered" };
	}
	return { kind: "failure" };
}

// ---------------------------------------------------------------------------
// request handler
// ---------------------------------------------------------------------------

export default {
	async fetch(request: Request, env: Env): Promise<Response> {
		const url = new URL(request.url);
		if (url.pathname !== "/v1/wake") {
			return jsonError(404, { error: "not_found" });
		}
		if (request.method !== "POST") {
			return jsonError(405, { error: "method_not_allowed" });
		}

		const contentType = (request.headers.get("content-type") ?? "").toLowerCase();
		if (!contentType.includes("application/json")) {
			return jsonError(400, { error: "malformed" });
		}
		const bodyText = await request.text();
		if (bodyText.length > MAX_BODY_BYTES) {
			return jsonError(400, { error: "malformed" });
		}
		let parsed: unknown;
		try {
			parsed = JSON.parse(bodyText);
		} catch {
			return jsonError(400, { error: "malformed" });
		}
		const wake = validateWake(parsed);
		if (wake === null) {
			return jsonError(400, { error: "malformed" });
		}

		// Rate limit per token, keyed by SHA-256 of the token — never the raw token.
		const tokenHash = await sha256Hex(wake.token);
		let limited = false;
		try {
			const rl = await env.WAKE_LIMIT.limit({ key: tokenHash });
			limited = !rl.success;
		} catch {
			// Rate-limit binding failure fails closed: without it an attacker could
			// push unlimited FCM sends, so reject rather than bypass.
			limited = true;
		}
		if (limited) {
			console.log(`wake ratelimited hash=${tokenHash.slice(0, TOKEN_HASH_LOG_PREFIX)}`);
			return jsonError(429, { error: "rate_limited" });
		}

		const sa = parseServiceAccount(env.FCM_SERVICE_ACCOUNT);
		if (sa === null) {
			console.log("wake auth_error status=500 (service account secret malformed)");
			return jsonError(500, { error: "auth_config" });
		}

		const auth = await getAccessToken(sa, fetch);
		if (auth === null) {
			console.log(`wake oauth_failure hash=${tokenHash.slice(0, TOKEN_HASH_LOG_PREFIX)}`);
			return jsonError(502, { error: "upstream" });
		}

		const outcome = await sendFcmWake(auth.projectId, auth.accessToken, wake, fetch);
		console.log(`wake fcm=${outcome.kind} hash=${tokenHash.slice(0, TOKEN_HASH_LOG_PREFIX)}`);
		switch (outcome.kind) {
			case "accepted":
				return jsonError(202, { ok: true });
			case "unregistered":
				// DESIGN addition: the desktop tells the user to reopen the phone app.
				return jsonError(410, { error: "unregistered" });
			case "failure":
				return jsonError(502, { error: "upstream" });
		}
	},
} satisfies ExportedHandler<Env>;

/** Test hook: clear the module-scope OAuth cache between tests. */
export function __clearTokenCacheForTests(): void {
	tokenCache.clear();
}
