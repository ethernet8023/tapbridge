import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import worker, { __clearTokenCacheForTests, type Env } from "../src/worker";

const OAUTH_URL = "https://oauth2.googleapis.com/token";
const FCM_URL = "https://fcm.googleapis.com/v1/projects/tapbridge-test/messages:send";

interface RecordedRequest {
	url: string;
	method: string;
	headers: Record<string, string>;
	body: string;
}

const oauthCalls: RecordedRequest[] = [];
const fcmCalls: RecordedRequest[] = [];
let fcmStatus = 200;
let fcmBody = "{}";
let oauthStatus = 200;

let saJson = "";
let envLike: Env;

// --- base64 helpers ----------------------------------------------------------
function bytesToBase64(bytes: Uint8Array): string {
	let bin = "";
	for (const b of bytes) bin += String.fromCharCode(b);
	return btoa(bin);
}

function base64ToBytes(b64: string): Uint8Array {
	const bin = atob(b64);
	const out = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
	return out;
}

function b64urlDecode(s: string): Uint8Array {
	const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
	const pad = b64.length % 4 === 0 ? "" : "=".repeat(4 - (b64.length % 4));
	return base64ToBytes(b64 + pad);
}

function utf8Decode(bytes: Uint8Array): string {
	return new TextDecoder().decode(bytes);
}

// --- throwaway RSA key -------------------------------------------------------
let signingPrivKey: CryptoKey;
let verifyPubKey: CryptoKey;

async function pemEncode(der: Uint8Array, label: string): Promise<string> {
	const b64 = bytesToBase64(der);
	const lines = b64.match(/.{1,64}/g) ?? [];
	return `-----BEGIN ${label}-----\n${lines.join("\n")}\n-----END ${label}-----`;
}

async function buildServiceAccountJson(): Promise<string> {
	const keyPair = (await crypto.subtle.generateKey(
		{ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
		true,
		["sign", "verify"],
	)) as {
		privateKey: CryptoKey;
		publicKey: CryptoKey;
	};
	signingPrivKey = keyPair.privateKey;
	const spki = (await crypto.subtle.exportKey("spki", keyPair.publicKey)) as ArrayBuffer;
	const spkiBytes = new Uint8Array(spki);
	verifyPubKey = await crypto.subtle.importKey(
		"spki",
		spkiBytes,
		{ name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
		false,
		["verify"],
	);
	const pkcs8 = (await crypto.subtle.exportKey("pkcs8", signingPrivKey)) as ArrayBuffer;
	const privPem = await pemEncode(new Uint8Array(pkcs8), "PRIVATE KEY");
	return JSON.stringify({
		type: "service_account",
		client_email: "relay@tapbridge-test.iam.gserviceaccount.com",
		private_key: privPem,
		project_id: "tapbridge-test",
	});
}

interface ParsedJwt {
	header: Record<string, unknown>;
	claims: Record<string, unknown>;
	validSignature: boolean;
}

async function parseJwtFromAssertion(assertion: string): Promise<ParsedJwt> {
	const [h, c, s] = assertion.split(".");
	const signingInput = `${h}.${c}`;
	const valid = await crypto.subtle.verify(
		"RSASSA-PKCS1-v1_5",
		verifyPubKey,
		b64urlDecode(s),
		new TextEncoder().encode(signingInput),
	);
	return {
		header: JSON.parse(utf8Decode(b64urlDecode(h))),
		claims: JSON.parse(utf8Decode(b64urlDecode(c))),
		validSignature: valid,
	};
}

function validWake(token = "a".repeat(152)) {
	return { token, host: "HOSTID123", hint: "HOSTID123;d=10.0.0.1:36412" };
}

async function postWake(body: unknown): Promise<Response> {
	return worker.fetch(
		new Request("https://relay.example.com/v1/wake", {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: typeof body === "string" ? body : JSON.stringify(body),
		}),
		envLike,
	);
}

/** In-memory RateLimit binding with a per-key counter (mirrors the binding contract). */
function makeRateLimit(limit: number) {
	const counters = new Map<string, number>();
	return {
		limit: async ({ key }: { key: string }) => {
			const n = (counters.get(key) ?? 0) + 1;
			counters.set(key, n);
			return { success: n <= limit };
		},
	};
}

beforeAll(async () => {
	saJson = await buildServiceAccountJson();
});

beforeEach(() => {
	__clearTokenCacheForTests();
	oauthCalls.length = 0;
	fcmCalls.length = 0;
	fcmStatus = 200;
	fcmBody = "{}";
	oauthStatus = 200;
	envLike = {
		FCM_SERVICE_ACCOUNT: saJson,
		WAKE_LIMIT: makeRateLimit(10) as unknown as Env["WAKE_LIMIT"],
	};
	vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
		const req = input instanceof Request ? input : new Request(String(input), init);
		const record = (into: RecordedRequest[]): RecordedRequest => {
			const entry: RecordedRequest = {
				url: req.url,
				method: req.method,
				headers: Object.fromEntries(req.headers.entries()),
				body: "",
			};
			into.push(entry);
			return entry;
		};
		if (req.url === OAUTH_URL) {
			const entry = record(oauthCalls);
			entry.body = await req.text();
			return new Response(
				oauthStatus === 200
					? JSON.stringify({ access_token: "test-access-token", expires_in: 3600 })
					: "nope",
				{ status: oauthStatus },
			);
		}
		if (req.url === FCM_URL) {
			const entry = record(fcmCalls);
			entry.body = await req.text();
			return new Response(fcmBody, { status: fcmStatus });
		}
		throw new Error(`unexpected outbound fetch: ${req.url}`);
	});
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe("routing and method", () => {
	it("404s other paths", async () => {
		const resp = await worker.fetch(
			new Request("https://relay.example.com/v1/other", { method: "POST", body: "{}" }),
			envLike,
		);
		expect(resp.status).toBe(404);
	});

	it("405s GET on the path", async () => {
		const resp = await worker.fetch(new Request("https://relay.example.com/v1/wake", { method: "GET" }), envLike);
		expect(resp.status).toBe(405);
		expect(oauthCalls.length).toBe(0);
	});
});

describe("validation matrix", () => {
	const badBodies: Array<[string, unknown]> = [
		["missing token", { host: "h", hint: "x" }],
		["missing host", { token: "t", hint: "x" }],
		["missing hint", { token: "t", host: "h" }],
		["token too long", { token: "t".repeat(4097), host: "h", hint: "x" }],
		["host too long", { token: "t", host: "h".repeat(129), hint: "x" }],
		["hint too long", { token: "t", host: "h", hint: "x".repeat(1025) }],
		["empty token", { token: "", host: "h", hint: "x" }],
		["empty host", { token: "t", host: "", hint: "x" }],
		["empty hint", { token: "t", host: "h", hint: "" }],
		["token not string", { token: 42, host: "h", hint: "x" }],
		["host not string", { token: "t", host: null, hint: "x" }],
		["hint not string", { token: "t", host: "h", hint: ["x"] }],
		["array body", ["token", "host", "hint"]],
		["null body", null],
	];
	for (const [name, body] of badBodies) {
		it(`400 on ${name}`, async () => {
			const resp = await postWake(body);
			expect(resp.status).toBe(400);
		});
	}

	it("400 on malformed JSON", async () => {
		const resp = await postWake("{not json");
		expect(resp.status).toBe(400);
	});

	it("400 on non-JSON content type", async () => {
		const resp = await worker.fetch(
			new Request("https://relay.example.com/v1/wake", {
				method: "POST",
				headers: { "content-type": "text/plain" },
				body: JSON.stringify(validWake()),
			}),
			envLike,
		);
		expect(resp.status).toBe(400);
	});

	it("400 on oversized body (>8 KiB)", async () => {
		const resp = await postWake({ token: "t".repeat(4200), host: "h", hint: "x" });
		expect(resp.status).toBe(400);
	});

	it("accepts a valid body end-to-end", async () => {
		const resp = await postWake(validWake());
		expect(resp.status).toBe(202);
		expect(((await resp.json()) as { ok?: boolean }).ok).toBe(true);
	});
});

describe("JWT / OAuth", () => {
	it("sends an RS256 JWT with the required claims, verifiable with the public key", async () => {
		await postWake(validWake());
		expect(oauthCalls.length).toBe(1);
		const params = new URLSearchParams(oauthCalls[0].body);
		expect(params.get("grant_type")).toBe("urn:ietf:params:oauth:grant-type:jwt-bearer");
		const jwt = await parseJwtFromAssertion(params.get("assertion") ?? "");
		expect(jwt.validSignature).toBe(true);
		expect(jwt.header).toEqual({ alg: "RS256", typ: "JWT" });
		const nowSec = Math.floor(Date.now() / 1000);
		expect(jwt.claims.iss).toBe("relay@tapbridge-test.iam.gserviceaccount.com");
		expect(jwt.claims.scope).toBe("https://www.googleapis.com/auth/firebase.messaging");
		expect(jwt.claims.aud).toBe("https://oauth2.googleapis.com/token");
		expect((jwt.claims.exp as number) - nowSec).toBeGreaterThan(3500);
		expect((jwt.claims.exp as number) - (jwt.claims.iat as number)).toBe(3600);
		expect(jwt.claims.exp as number).toBeLessThanOrEqual(nowSec + 3600);
		expect(oauthCalls[0].headers["content-type"]).toBe("application/x-www-form-urlencoded");
	});

	it("caches the access token: two wakes, one OAuth call, same bearer", async () => {
		await postWake(validWake());
		await postWake(validWake());
		expect(oauthCalls.length).toBe(1);
		expect(fcmCalls.length).toBe(2);
		expect(fcmCalls[0].headers.authorization).toBe("Bearer test-access-token");
		expect(fcmCalls[1].headers.authorization).toBe("Bearer test-access-token");
	});
});

describe("FCM payload", () => {
	it("sends exactly the data-only HIGH-priority wake shape", async () => {
		await postWake(validWake());
		expect(fcmCalls.length).toBe(1);
		expect(fcmCalls[0].headers["content-type"]).toBe("application/json");
		const sent = JSON.parse(fcmCalls[0].body);
		expect(sent).toEqual({
			message: {
				token: "a".repeat(152),
				data: { t: "wake", host: "HOSTID123", hint: "HOSTID123;d=10.0.0.1:36412" },
				android: { priority: "HIGH", ttl: "60s" },
			},
		});
		expect(sent.message.notification).toBeUndefined();
	});
});

describe("status mapping", () => {
	it("maps FCM 404 to 410 unregistered", async () => {
		fcmStatus = 404;
		const resp = await postWake(validWake());
		expect(resp.status).toBe(410);
		expect(((await resp.json()) as { error?: string }).error).toBe("unregistered");
	});

	it("maps FCM 400 INVALID_ARGUMENT to 410 unregistered", async () => {
		fcmStatus = 400;
		fcmBody = JSON.stringify({ error: { code: 400, details: [{ errorType: "INVALID_ARGUMENT" }] } });
		const resp = await postWake(validWake());
		expect(resp.status).toBe(410);
		expect(((await resp.json()) as { error?: string }).error).toBe("unregistered");
	});

	it("maps FCM 500 to 502", async () => {
		fcmStatus = 500;
		const resp = await postWake(validWake());
		expect(resp.status).toBe(502);
	});

	it("maps OAuth failure to 502", async () => {
		oauthStatus = 500;
		const resp = await postWake(validWake());
		expect(resp.status).toBe(502);
	});

	it("rate limits the 11th request per token (10/min), 429", async () => {
		const token = "ratelimited-token-0001";
		for (let i = 0; i < 10; i++) {
			const resp = await postWake(validWake(token));
			expect(resp.status).toBe(202);
		}
		const resp11 = await postWake(validWake(token));
		expect(resp11.status).toBe(429);
		expect(fcmCalls.length).toBe(10);
	});

	it("rate limit is per token: a second token is unaffected", async () => {
		for (let i = 0; i < 10; i++) {
			await postWake(validWake("token-one-000000000000"));
		}
		const resp = await postWake(validWake("token-two-000000000000"));
		expect(resp.status).toBe(202);
	});
});

describe("no secret leakage", () => {
	it("logs contain no token, hint, or secret text", async () => {
		const logs: string[] = [];
		const origLog = console.log;
		console.log = (...args: unknown[]) => {
			logs.push(args.map(String).join(" "));
		};
		try {
			const token = "leakcheck-token-0000000000000000";
			await postWake(validWake(token));
			fcmStatus = 404;
			await postWake(validWake(token));
			fcmStatus = 502;
			await postWake(validWake(token));
		} finally {
			console.log = origLog;
		}
		const joined = logs.join("\n");
		expect(joined).not.toContain("leakcheck-token");
		expect(joined).not.toContain("10.0.0.1:36412");
		expect(joined).not.toContain("PRIVATE KEY");
		expect(joined).not.toContain("test-access-token");
		expect(joined).toMatch(/hash=[0-9a-f]{1,16}/);
	});
});

describe("worker export sanity", () => {
	it("module exports a fetch handler", () => {
		expect(typeof (worker as { fetch?: unknown }).fetch).toBe("function");
	});
});
