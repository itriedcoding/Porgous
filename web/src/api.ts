export type Session = {
  user: { id: string; email: string; username: string; root_admin: boolean };
  csrf_token: string | null;
  is_admin: boolean;
};

let csrfToken: string | null = sessionStorage.getItem("porgous_csrf");

export function setCsrf(token: string | null) {
  csrfToken = token;
  if (token) sessionStorage.setItem("porgous_csrf", token);
  else sessionStorage.removeItem("porgous_csrf");
}

export class ApiFailure extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export async function api<T = any>(path: string, options: RequestInit = {}): Promise<T> {
  const headers = new Headers(options.headers);
  if (options.body && !(options.body instanceof FormData)) {
    headers.set("Content-Type", "application/json");
  }
  if (csrfToken && options.method && !["GET", "HEAD"].includes(options.method.toUpperCase())) {
    headers.set("x-csrf-token", csrfToken);
  }
  const response = await fetch(`/api${path}`, { ...options, headers, credentials: "same-origin" });
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  let data: any = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = null;
  }
  if (!response.ok) {
    const message = data?.error?.message || `Request failed (${response.status})`;
    throw new ApiFailure(response.status, message);
  }
  return data as T;
}

export function wsUrl(path: string): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}/api${path}`;
}
