import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiFailure, Session, setCsrf, wsUrl } from "./api";

type Page = "overview" | "servers" | "nodes" | "users" | "account";

const STATUS_STYLES: Record<string, string> = {
  running: "bg-emerald-500/15 text-emerald-300",
  starting: "bg-amber-500/15 text-amber-300",
  stopping: "bg-amber-500/15 text-amber-300",
  offline: "bg-slate-500/20 text-slate-300",
  installing: "bg-sky-500/15 text-sky-300",
  creating: "bg-sky-500/15 text-sky-300",
  suspended: "bg-rose-500/15 text-rose-300",
};

function bytes(n: number | undefined | null) {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i++;
  }
  return `${value.toFixed(value >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

function stripAnsi(text: string) {
  return text.replace(/\u001b\[[0-9;]*[A-Za-z]/g, "");
}

function Status({ status }: { status: string }) {
  return <span className={`badge ${STATUS_STYLES[status] || "bg-slate-500/20 text-slate-300"}`}>{status}</span>;
}

function ErrorBox({ message }: { message: string | null }) {
  if (!message) return null;
  return <div className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-sm text-rose-200">{message}</div>;
}

function Login({ onLogin }: { onLogin: (session: Session) => void }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [totp, setTotp] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await api<{ user: Session["user"]; csrf_token: string }>("/auth/login", {
        method: "POST",
        body: JSON.stringify({ email, password, totp: totp || undefined }),
      });
      setCsrf(result.csrf_token);
      onLogin({ user: result.user, csrf_token: result.csrf_token, is_admin: result.user.root_admin });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Login failed");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center p-6">
      <form onSubmit={submit} className="card w-full max-w-md space-y-4">
        <div>
          <h1 className="text-2xl font-semibold text-white">Porgous</h1>
          <p className="mt-1 text-sm text-slate-400">Sign in to manage your game servers.</p>
        </div>
        <ErrorBox message={error} />
        <div>
          <label className="label">Email</label>
          <input className="input" type="email" required value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="username" />
        </div>
        <div>
          <label className="label">Password</label>
          <input className="input" type="password" required value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" />
        </div>
        <div>
          <label className="label">Two-factor code (if enabled)</label>
          <input className="input" inputMode="numeric" value={totp} onChange={(e) => setTotp(e.target.value)} placeholder="123456" />
        </div>
        <button className="btn btn-primary w-full" disabled={busy}>{busy ? "Signing in…" : "Sign in"}</button>
      </form>
    </div>
  );
}

function Overview({ onOpen }: { onOpen: (id: string) => void }) {
  const [servers, setServers] = useState<any[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api<{ servers: any[] }>("/servers").then((r) => setServers(r.servers)).catch((e) => setError(e.message));
  }, []);
  const counts = useMemo(() => {
    const by: Record<string, number> = {};
    servers.forEach((s) => (by[s.server.status] = (by[s.server.status] || 0) + 1));
    return by;
  }, [servers]);
  return (
    <div className="space-y-6">
      <ErrorBox message={error} />
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <div className="card"><div className="text-xs uppercase text-slate-400">Servers</div><div className="mt-2 text-3xl font-semibold text-white">{servers.length}</div></div>
        <div className="card"><div className="text-xs uppercase text-slate-400">Running</div><div className="mt-2 text-3xl font-semibold text-emerald-300">{counts.running || 0}</div></div>
        <div className="card"><div className="text-xs uppercase text-slate-400">Offline</div><div className="mt-2 text-3xl font-semibold text-slate-300">{counts.offline || 0}</div></div>
        <div className="card"><div className="text-xs uppercase text-slate-400">Memory allocated</div><div className="mt-2 text-3xl font-semibold text-white">{bytes(servers.reduce((a, s) => a + s.server.memory_mb * 1024 * 1024, 0))}</div></div>
      </div>
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {servers.map((entry) => (
          <button key={entry.server.id} onClick={() => onOpen(entry.server.id)} className="card text-left transition hover:border-accent/50">
            <div className="flex items-center justify-between">
              <h3 className="font-medium text-white">{entry.server.name}</h3>
              <Status status={entry.server.status} />
            </div>
            <p className="mt-1 text-sm text-slate-400">{entry.egg_name} · {entry.node_name}</p>
            <div className="mt-4 flex gap-4 text-xs text-slate-400">
              <span>{entry.server.memory_mb} MB RAM</span>
              <span>{entry.server.cpu_percent}% CPU</span>
              <span>{entry.primary_allocation || "no allocation"}</span>
            </div>
          </button>
        ))}
        {servers.length === 0 && <div className="card text-sm text-slate-400">No servers yet. Create one from the Servers page.</div>}
      </div>
    </div>
  );
}

function CreateServer({ onCreated, onClose }: { onCreated: () => void; onClose: () => void }) {
  const [eggs, setEggs] = useState<any[]>([]);
  const [nodes, setNodes] = useState<any[]>([]);
  const [form, setForm] = useState({ name: "", egg_id: "", node_id: "", memory_mb: 1024, disk_mb: 5120, cpu_percent: 100, image: "" });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api<{ eggs: any[] }>("/eggs").then((r) => {
      setEggs(r.eggs);
      if (r.eggs[0]) setForm((f) => ({ ...f, egg_id: r.eggs[0].id }));
    }).catch((e) => setError(e.message));
    api<{ nodes: any[] }>("/nodes").then((r) => {
      setNodes(r.nodes);
      if (r.nodes[0]) setForm((f) => ({ ...f, node_id: r.nodes[0].id }));
    }).catch(() => {});
  }, []);

  const egg = eggs.find((e) => e.id === form.egg_id);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api("/servers", {
        method: "POST",
        body: JSON.stringify({
          name: form.name,
          egg_id: form.egg_id,
          node_id: form.node_id || undefined,
          memory_mb: Number(form.memory_mb),
          disk_mb: Number(form.disk_mb),
          cpu_percent: Number(form.cpu_percent),
          image: form.image || undefined,
        }),
      });
      onCreated();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not create the server");
    } finally {
      setBusy(false);
    }
  }

  return (
    <form onSubmit={submit} className="card space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-medium text-white">New server</h2>
        <button type="button" className="btn" onClick={onClose}>Close</button>
      </div>
      <ErrorBox message={error} />
      <div className="grid gap-4 md:grid-cols-2">
        <div><label className="label">Name</label><input className="input" required minLength={3} maxLength={100} value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} /></div>
        <div><label className="label">Template</label>
          <select className="input" value={form.egg_id} onChange={(e) => setForm({ ...form, egg_id: e.target.value, image: "" })}>
            {eggs.map((e) => <option key={e.id} value={e.id}>{e.name}</option>)}
          </select>
        </div>
        <div><label className="label">Node</label>
          <select className="input" value={form.node_id} onChange={(e) => setForm({ ...form, node_id: e.target.value })}>
            {nodes.map((n) => <option key={n.id} value={n.id}>{n.name}</option>)}
          </select>
        </div>
        <div><label className="label">Docker image</label>
          <select className="input" value={form.image || egg?.docker_images?.[0] || ""} onChange={(e) => setForm({ ...form, image: e.target.value })}>
            {(egg?.docker_images || []).map((image: string) => <option key={image} value={image}>{image}</option>)}
          </select>
        </div>
        <div><label className="label">Memory (MB)</label><input className="input" type="number" min={128} value={form.memory_mb} onChange={(e) => setForm({ ...form, memory_mb: +e.target.value })} /></div>
        <div><label className="label">Disk (MB)</label><input className="input" type="number" min={256} value={form.disk_mb} onChange={(e) => setForm({ ...form, disk_mb: +e.target.value })} /></div>
        <div><label className="label">CPU (%)</label><input className="input" type="number" min={10} max={25600} value={form.cpu_percent} onChange={(e) => setForm({ ...form, cpu_percent: +e.target.value })} /></div>
      </div>
      <button className="btn btn-primary" disabled={busy || !form.egg_id}>{busy ? "Creating…" : "Create server"}</button>
    </form>
  );
}

function Console({ id }: { id: string }) {
  const [lines, setLines] = useState<string[]>([]);
  const [command, setCommand] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const socket = new WebSocket(wsUrl(`/servers/${id}/ws`));
    socket.onmessage = (event) => {
      let text = event.data as string;
      try {
        const data = JSON.parse(text);
        text = data.line ?? data.data ?? data.message ?? JSON.stringify(data);
      } catch { /* plain text */ }
      setLines((current) => [...current.slice(-999), stripAnsi(String(text))]);
    };
    socket.onerror = () => setLines((current) => [...current, "[connection error]"]);
    return () => socket.close();
  }, [id]);
  useEffect(() => {
    if (box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [lines]);
  async function send(e: React.FormEvent) {
    e.preventDefault();
    if (!command.trim()) return;
    try {
      await api(`/servers/${id}/command`, { method: "POST", body: JSON.stringify({ command }) });
      setCommand("");
    } catch (err) {
      setLines((current) => [...current, `[error] ${err instanceof Error ? err.message : "command failed"}`]);
    }
  }
  return (
    <div className="space-y-3">
      <div ref={box} className="h-80 overflow-auto rounded-lg border border-edge bg-black/60 p-3 font-mono text-xs leading-relaxed text-slate-200">
        {lines.length ? lines.map((line, i) => <div key={i}>{line}</div>) : <span className="text-slate-500">Waiting for console output…</span>}
      </div>
      <form onSubmit={send} className="flex gap-2">
        <input className="input font-mono" placeholder="Type a command and press Enter" value={command} onChange={(e) => setCommand(e.target.value)} />
        <button className="btn">Send</button>
      </form>
    </div>
  );
}

function Files({ id }: { id: string }) {
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<any[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ path: string; content: string } | null>(null);

  const load = useCallback(async (target: string) => {
    try {
      const result = await api<{ path: string; entries: any[] }>(`/servers/${id}/files?path=${encodeURIComponent(target)}`);
      setPath(result.path);
      setEntries(result.entries);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not list files");
    }
  }, [id]);

  useEffect(() => { void load(""); }, [load]);

  async function open(entry: any) {
    if (entry.is_dir) return load(entry.path);
    if (entry.size > 2 * 1024 * 1024) return setError("File is too large to edit (>2 MB).");
    try {
      const result = await api<{ content: string }>(`/servers/${id}/files/content?path=${encodeURIComponent(entry.path)}`);
      setEditing({ path: entry.path, content: result.content });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not read the file");
    }
  }

  async function save() {
    if (!editing) return;
    try {
      await api(`/servers/${id}/files/content`, { method: "PUT", body: JSON.stringify(editing) });
      setEditing(null);
      setError(null);
      void load(path);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not save the file");
    }
  }

  const parent = path.split("/").slice(0, -1).join("/");
  return (
    <div className="space-y-3">
      <ErrorBox message={error} />
      {editing ? (
        <div className="space-y-2">
          <div className="font-mono text-xs text-slate-400">{editing.path}</div>
          <textarea className="input h-72 font-mono text-xs" value={editing.content} onChange={(e) => setEditing({ ...editing, content: e.target.value })} />
          <div className="flex gap-2"><button className="btn btn-primary" onClick={save}>Save</button><button className="btn" onClick={() => setEditing(null)}>Cancel</button></div>
        </div>
      ) : (
        <>
          <div className="flex items-center gap-2 text-sm">
            <button className="btn" onClick={() => load(parent)} disabled={!path}>Up</button>
            <span className="truncate font-mono text-xs text-slate-400">/{path}</span>
          </div>
          <div className="overflow-hidden rounded-lg border border-edge">
            {entries.map((entry) => (
              <button key={entry.path} onClick={() => open(entry)} className="flex w-full items-center justify-between border-b border-edge/60 px-3 py-2 text-left text-sm last:border-0 hover:bg-white/5">
                <span className="truncate">{entry.is_dir ? "📁" : "📄"} {entry.name}</span>
                <span className="text-xs text-slate-500">{entry.is_dir ? "" : bytes(entry.size)}</span>
              </button>
            ))}
            {entries.length === 0 && <div className="px-3 py-4 text-sm text-slate-500">This folder is empty.</div>}
          </div>
        </>
      )}
    </div>
  );
}

function Backups({ id }: { id: string }) {
  const [backups, setBackups] = useState<any[]>([]);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const load = useCallback(() => {
    api<{ backups: any[] }>(`/servers/${id}/backups`).then((r) => setBackups(r.backups)).catch((e) => setError(e.message));
  }, [id]);
  useEffect(load, [load]);
  async function create(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      await api(`/servers/${id}/backups`, { method: "POST", body: JSON.stringify({ name }) });
      setName("");
      load();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Backup failed");
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="space-y-3">
      <ErrorBox message={error} />
      <form onSubmit={create} className="flex gap-2">
        <input className="input" placeholder="Backup name (optional)" value={name} onChange={(e) => setName(e.target.value)} />
        <button className="btn btn-primary" disabled={busy}>{busy ? "Creating…" : "Create backup"}</button>
      </form>
      <div className="space-y-2">
        {backups.map((b) => (
          <div key={b.id} className="flex items-center justify-between rounded-lg border border-edge px-3 py-2 text-sm">
            <div>
              <div className="text-white">{b.name || b.uuid}</div>
              <div className="text-xs text-slate-500">{bytes(b.size_bytes)} · {new Date(b.created_at).toLocaleString()} · {b.status}</div>
            </div>
            <a className="btn" href={`/api/servers/${id}/backups/${b.id}/download`}>Download</a>
          </div>
        ))}
        {backups.length === 0 && <div className="text-sm text-slate-500">No backups yet.</div>}
      </div>
    </div>
  );
}

function ServerDetail({ id, onBack }: { id: string; onBack: () => void }) {
  const [detail, setDetail] = useState<any>(null);
  const [tab, setTab] = useState<"console" | "files" | "backups">("console");
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api(`/servers/${id}`).then(setDetail).catch((e) => setError(e.message));
  }, [id]);
  useEffect(load, [load]);

  async function power(action: string) {
    try {
      await api(`/servers/${id}/power`, { method: "POST", body: JSON.stringify({ action }) });
      setTimeout(load, 800);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Power action failed");
    }
  }

  if (!detail) return <div className="card"><ErrorBox message={error} /><button className="btn" onClick={onBack}>Back</button></div>;
  const server = detail.server;
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <button className="btn" onClick={onBack}>Back</button>
          <div>
            <h2 className="text-xl font-semibold text-white">{server.name}</h2>
            <p className="text-xs text-slate-400">{detail.egg_name} · {detail.node_name} · {detail.primary_allocation || "no allocation"}</p>
          </div>
          <Status status={server.status} />
        </div>
        <div className="flex gap-2">
          <button className="btn" onClick={() => power("start")}>Start</button>
          <button className="btn" onClick={() => power("stop")}>Stop</button>
          <button className="btn" onClick={() => power("restart")}>Restart</button>
          <button className="btn" onClick={() => power("kill")}>Kill</button>
        </div>
      </div>
      <ErrorBox message={error} />
      <div className="grid gap-3 sm:grid-cols-4">
        <div className="card"><div className="text-xs text-slate-400">Memory</div><div className="text-lg text-white">{server.memory_mb} MB</div></div>
        <div className="card"><div className="text-xs text-slate-400">CPU</div><div className="text-lg text-white">{server.cpu_percent}%</div></div>
        <div className="card"><div className="text-xs text-slate-400">Disk</div><div className="text-lg text-white">{server.disk_mb} MB</div></div>
        <div className="card"><div className="text-xs text-slate-400">Uptime</div><div className="text-lg text-white">{Math.floor((detail.uptime_seconds || 0) / 60)} min</div></div>
      </div>
      <div className="flex gap-2">
        {(["console", "files", "backups"] as const).map((item) => (
          <button key={item} className={`btn ${tab === item ? "btn-primary" : ""}`} onClick={() => setTab(item)}>{item}</button>
        ))}
      </div>
      <div className="card">
        {tab === "console" && <Console id={id} />}
        {tab === "files" && <Files id={id} />}
        {tab === "backups" && <Backups id={id} />}
      </div>
    </div>
  );
}

function ServersPage({ initial, onClearInitial }: { initial: string | null; onClearInitial: () => void }) {
  const [servers, setServers] = useState<any[]>([]);
  const [open, setOpen] = useState<string | null>(initial);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    api<{ servers: any[] }>("/servers").then((r) => setServers(r.servers)).catch((e) => setError(e.message));
  }, []);
  useEffect(load, [load]);
  useEffect(() => { if (initial) { setOpen(initial); onClearInitial(); } }, [initial, onClearInitial]);
  if (open) return <ServerDetail id={open} onBack={() => { setOpen(null); load(); }} />;
  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-semibold text-white">Servers</h2>
        <button className="btn btn-primary" onClick={() => setCreating(!creating)}>New server</button>
      </div>
      {creating && <CreateServer onClose={() => setCreating(false)} onCreated={() => { setCreating(false); load(); }} />}
      <ErrorBox message={error} />
      <div className="space-y-2">
        {servers.map((entry) => (
          <button key={entry.server.id} onClick={() => setOpen(entry.server.id)} className="card flex w-full flex-wrap items-center justify-between gap-3 text-left hover:border-accent/50">
            <div>
              <div className="font-medium text-white">{entry.server.name}</div>
              <div className="text-xs text-slate-400">{entry.egg_name} · {entry.node_name} · {entry.owner_name}</div>
            </div>
            <div className="flex items-center gap-4 text-xs text-slate-400">
              <span>{entry.primary_allocation}</span>
              <Status status={entry.server.status} />
            </div>
          </button>
        ))}
        {servers.length === 0 && <div className="card text-sm text-slate-400">No servers yet.</div>}
      </div>
    </div>
  );
}

function NodesPage() {
  const [nodes, setNodes] = useState<any[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { api<{ nodes: any[] }>("/nodes").then((r) => setNodes(r.nodes)).catch((e) => setError(e.message)); }, []);
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold text-white">Nodes</h2>
      <ErrorBox message={error} />
      <div className="grid gap-3 md:grid-cols-2">
        {nodes.map((node) => (
          <div key={node.id} className="card space-y-2">
            <div className="flex items-center justify-between"><div className="font-medium text-white">{node.name}</div><span className="badge bg-sky-500/15 text-sky-300">{node.is_local ? "local" : "remote"}</span></div>
            <div className="text-xs text-slate-400">{node.fqdn}:{node.port} · {node.scheme}</div>
            <div className="text-sm text-slate-300">{node.server_count ?? 0} servers · {node.memory_mb ? bytes(node.memory_mb * 1024 * 1024) : "unlimited"} RAM</div>
            {node.maintenance && <div className="text-xs text-amber-300">Maintenance mode</div>}
          </div>
        ))}
      </div>
    </div>
  );
}

function UsersPage() {
  const [users, setUsers] = useState<any[]>([]);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { api<{ users: any[] }>("/users").then((r) => setUsers(r.users)).catch((e) => setError(e.message)); }, []);
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold text-white">Users</h2>
      <ErrorBox message={error} />
      <div className="overflow-hidden rounded-xl border border-edge">
        <table className="w-full text-left text-sm">
          <thead className="bg-panel text-xs uppercase text-slate-400"><tr><th className="px-4 py-2">Username</th><th className="px-4 py-2">Email</th><th className="px-4 py-2">Role</th><th className="px-4 py-2">Status</th></tr></thead>
          <tbody>
            {users.map((user) => (
              <tr key={user.id} className="border-t border-edge/60">
                <td className="px-4 py-2 text-white">{user.username}</td>
                <td className="px-4 py-2 text-slate-300">{user.email}</td>
                <td className="px-4 py-2">{user.root_admin ? "Administrator" : "User"}</td>
                <td className="px-4 py-2">{user.suspended ? "Suspended" : "Active"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function AccountPage({ session, onLogout }: { session: Session; onLogout: () => void }) {
  return (
    <div className="max-w-xl space-y-4">
      <h2 className="text-xl font-semibold text-white">Account</h2>
      <div className="card space-y-2 text-sm">
        <div><span className="text-slate-400">Username:</span> {session.user.username}</div>
        <div><span className="text-slate-400">Email:</span> {session.user.email}</div>
        <div><span className="text-slate-400">Role:</span> {session.is_admin ? "Administrator" : "User"}</div>
        <div><span className="text-slate-400">Two-factor:</span> manage from the API or CLI</div>
      </div>
      <button className="btn" onClick={onLogout}>Sign out</button>
    </div>
  );
}

export default function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [checking, setChecking] = useState(true);
  const [page, setPage] = useState<Page>("overview");
  const [openServer, setOpenServer] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const current = await api<Session>("/auth/session");
      if (current.csrf_token) setCsrf(current.csrf_token);
      setSession(current);
    } catch (err) {
      if (!(err instanceof ApiFailure) || err.status !== 401) console.warn(err);
      setSession(null);
    } finally {
      setChecking(false);
    }
  }, []);

  useEffect(() => { void refresh(); }, [refresh]);

  async function logout() {
    try { await api("/auth/logout", { method: "POST" }); } catch { /* ignore */ }
    setCsrf(null);
    setSession(null);
    setPage("overview");
  }

  if (checking) return <div className="flex min-h-screen items-center justify-center text-sm text-slate-400">Loading…</div>;
  if (!session) return <Login onLogin={(value) => { setSession(value); setPage("overview"); }} />;

  const nav: { id: Page; label: string; admin?: boolean }[] = [
    { id: "overview", label: "Overview" },
    { id: "servers", label: "Servers" },
    { id: "nodes", label: "Nodes", admin: true },
    { id: "users", label: "Users", admin: true },
    { id: "account", label: "Account" },
  ];

  return (
    <div className="min-h-screen md:flex">
      <aside className="border-b border-edge bg-panel/60 p-4 md:min-h-screen md:w-60 md:border-b-0 md:border-r">
        <div className="mb-6 flex items-center gap-2">
          <div className="grid h-9 w-9 place-items-center rounded-lg bg-accent/20 font-bold text-accent">P</div>
          <div><div className="font-semibold text-white">Porgous</div><div className="text-xs text-slate-400">server management</div></div>
        </div>
        <nav className="flex gap-1 overflow-x-auto md:flex-col">
          {nav.filter((item) => !item.admin || session.is_admin).map((item) => (
            <button key={item.id} onClick={() => { setPage(item.id); setOpenServer(null); }} className={`rounded-lg px-3 py-2 text-left text-sm ${page === item.id ? "bg-accent/15 text-white" : "text-slate-400 hover:text-white"}`}>
              {item.label}
            </button>
          ))}
        </nav>
      </aside>
      <main className="flex-1 p-4 md:p-8">
        <div className="mx-auto max-w-6xl">
          {page === "overview" && <Overview onOpen={(id) => { setOpenServer(id); setPage("servers"); }} />}
          {page === "servers" && <ServersPage initial={openServer} onClearInitial={() => setOpenServer(null)} />}
          {page === "nodes" && session.is_admin && <NodesPage />}
          {page === "users" && session.is_admin && <UsersPage />}
          {page === "account" && <AccountPage session={session} onLogout={logout} />}
        </div>
      </main>
    </div>
  );
}
