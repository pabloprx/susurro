// DESIGN 6.3. Two backend kinds (Word, Snippet), three tabs (Words, Corrections = Word aliases, Snippets).
import { useEffect, useMemo, useRef, useState } from "react";
import { AlertCircle, ArrowDownToLine, ArrowRight, ArrowUpFromLine, Pencil, Plus, Search, Star, Trash2 } from "lucide-react";
import { api, type DictEntry, type DictEntryInput } from "../lib/api";
import { settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Empty, Label, Popover, Segmented, Sheet, Title, toast } from "../ui";

type Tab = "words" | "corrections" | "snippets";
const norm = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
const input = (e: DictEntry): DictEntryInput => ({ id: e.id, kind: e.kind, text: e.text, aliases: e.aliases, expansion: e.expansion, exact_only: e.exact_only, starred: e.starred });
const fresh = (kind: DictEntry["kind"], text: string, aliases: string[] = [], expansion: string | null = null): DictEntryInput => ({ id: null, kind, text, aliases, expansion, exact_only: false, starred: false });
/** Rank used for biasing: starred, then uses, then newest (PLAN 2.4). */
const rank = (a: DictEntry, b: DictEntry) => +b.starred - +a.starred || b.uses - a.uses || b.created_at - a.created_at;

export default function Dictionary() {
  const [list, setList] = useState<DictEntry[] | null>(null);
  const [tab, setTab] = useState<Tab>("words");
  const [filter, setFilter] = useState("");
  const [lit, setLit] = useState<string | null>(null); // row key that just got added
  const [sheet, setSheet] = useState<DictEntry | "new" | null>(null);
  const addRef = useRef<HTMLInputElement>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const settings = settingsStore.use();

  const load = () => api.dictList().then(setList);
  useEffect(() => void load(), []);
  useEffect(() => {
    // Arrow keys on the tab radios keep focus there (DESIGN 9.4); a click or the first load focuses the add field.
    if (tab !== "snippets" && document.activeElement?.getAttribute("role") !== "radio") addRef.current?.focus();
  }, [tab]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (!e.metaKey) return;
      if (e.key === "f") (e.preventDefault(), filterRef.current?.focus());
      if (e.key === "n") (e.preventDefault(), tab === "snippets" ? setSheet("new") : addRef.current?.focus());
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [tab]);

  const all = list ?? [];
  const wordsList = all.filter((e) => e.kind === "word").sort(rank);
  const snippets = all.filter((e) => e.kind === "snippet").sort(rank);
  const corrections = wordsList.flatMap((e) => e.aliases.map((a) => ({ alias: a, entry: e })));
  const f = norm(filter);
  const match = (...s: (string | null)[]) => !f || s.some((x) => x && norm(x).includes(f));
  const shownWords = wordsList.filter((w) => match(w.text));
  const shownCorrections = corrections.filter((c) => match(c.alias, c.entry.text));
  const shownSnippets = snippets.filter((s) => match(s.text, s.expansion));
  const noMatch = (
    <Empty line={[`No match for “${filter}”.`]} help="The filter checks terms, corrections and expansions." action={<button className="btn secondary" onClick={() => setFilter("")}>Clear filter</button>} />
  );

  /** Where a term already lives, for the shared-namespace check. */
  const whereIs = (term: string, ignore?: number): string | null => {
    const n = norm(term);
    if (!n) return null;
    for (const e of all) {
      if (e.id === ignore) continue;
      if (norm(e.text) === n) return e.kind === "snippet" ? "Already in Snippets" : "Already in Words";
      if (e.aliases.some((a) => norm(a) === n)) return `Already in Corrections (→ ${e.text})`;
    }
    return null;
  };
  const check = (term: string, ignore?: number): string | null => {
    const t = term.trim();
    if (!t) return null;
    if (t.length > 60) return "Max 60 characters";
    if (!/[\p{L}\p{N}]/u.test(t)) return "Needs at least one letter or number";
    return whereIs(t, ignore);
  };

  async function upsert(e: DictEntryInput, litKey?: string): Promise<string | null> {
    try {
      await api.dictUpsert(e);
      await load();
      if (litKey) setLit(litKey);
      return null;
    } catch (err) {
      return String(err);
    }
  }

  function remove(e: DictEntry) {
    api.dictDelete([e.id]).then(load);
    toast(`Deleted “${e.text}”`, { actions: [{ label: "Undo", run: () => upsert({ ...input(e), id: null }) }] });
  }
  function removeAlias(e: DictEntry, alias: string) {
    upsert({ ...input(e), aliases: e.aliases.filter((a) => a !== alias) });
    toast(`Deleted “${alias} → ${e.text}”`, { actions: [{ label: "Undo", run: () => api.dictList().then((l) => { const cur = l.find((x) => x.id === e.id); if (cur) upsert({ ...input(cur), aliases: [...cur.aliases, alias] }); }) }] });
  }

  const exportJson = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([JSON.stringify(all, null, 2)], { type: "application/json" }));
    a.download = "susurro-dictionary.json";
    a.click();
  };
  const importJson = async (file: File) => {
    let rows: DictEntry[];
    try {
      rows = JSON.parse(await file.text());
      if (!Array.isArray(rows)) throw 0;
    } catch {
      return toast("That file is not a susurro dictionary export (JSON list).", { kind: "error" });
    }
    let added = 0;
    let dup = 0;
    let bad = 0;
    for (const r of rows) {
      try {
        await api.dictUpsert(fresh(r.kind === "snippet" ? "snippet" : "word", String(r.text ?? ""), Array.isArray(r.aliases) ? r.aliases : [], r.expansion ?? null));
        added++;
      } catch (e) {
        if (/already|exists|duplicate/i.test(String(e))) dup++;
        else bad++;
      }
    }
    await load();
    toast(`${added} new · ${dup} duplicates skipped · ${bad} invalid`, { kind: added ? "success" : "info" });
  };

  // biasing footer: tells the truth about limits (whisper prompt budget is estimated at chars/4)
  const bias = useMemo(() => {
    let budget = 200 - 7; // seed sentence
    const fit: string[] = [];
    const left: DictEntry[] = [];
    for (const w of wordsList) {
      const cost = Math.ceil((w.text.length + 2) / 4);
      if (cost <= budget) (budget -= cost), fit.push(w.text);
      else left.push(w);
    }
    return { fit: fit.length, left };
  }, [list]);

  return (
    <>
      <Title
        parts={S.title.dictionary}
        right={
          <>
            <button className="btn ghost sm" onClick={() => fileRef.current?.click()}>
              <ArrowDownToLine size={14} strokeWidth={1.5} /> Import
            </button>
            <button className="btn ghost sm" onClick={exportJson} disabled={!all.length}>
              <ArrowUpFromLine size={14} strokeWidth={1.5} /> Export
            </button>
            <input ref={fileRef} type="file" accept="application/json,.json" hidden onChange={(e) => e.target.files?.[0] && importJson(e.target.files[0])} />
          </>
        }
      />
      <div className="dict-bar">
        <Segmented
          label="Dictionary section"
          value={tab}
          onChange={setTab}
          options={[
            { value: "words", label: <>Words <span className="count">{wordsList.length}</span></> },
            { value: "corrections", label: <>Corrections <span className="count">{corrections.length}</span></> },
            { value: "snippets", label: <>Snippets <span className="count">{snippets.length}</span></> },
          ]}
        />
        <label className="search">
          <Search size={15} strokeWidth={1.5} />
          <input ref={filterRef} className="input" placeholder="Filter…" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter entries" />
        </label>
      </div>

      {tab === "words" && (
        <>
          <Label>Words - susurro hints these to every provider</Label>
          <div className="card">
            <AddWord inputRef={addRef} check={check} onAdd={(t) => upsert(fresh("word", t), `w${norm(t)}`)} />
            {!wordsList.length ? (
              <Empty line={S.empty.words.line} help={S.empty.words.help} />
            ) : !shownWords.length ? (
              noMatch
            ) : (
              <div className="rows">
                {shownWords.map((w) => (
                  <EditableRow
                    key={w.id}
                    lit={lit === `w${norm(w.text)}`}
                    value={w.text}
                    check={(v) => check(v, w.id)}
                    onSave={(v) => upsert({ ...input(w), text: v })}
                    meta={`used ${w.uses}×`}
                    lead={
                      <button className="icon-btn sm star" aria-label={w.starred ? "Unstar" : "Star: hint first"} aria-pressed={w.starred} data-tip={w.starred ? "Starred: hinted first" : "Star to hint first"} onClick={() => upsert({ ...input(w), starred: !w.starred })}>
                        <Star size={13} strokeWidth={1.5} fill={w.starred ? "currentColor" : "none"} />
                      </button>
                    }
                    onDelete={() => remove(w)}
                  />
                ))}
              </div>
            )}
          </div>
          {!!wordsList.length && settings && (
            <div className="biasing">
              <span>BIASING:</span>
              <Popover
                align="left"
                up
                trigger={(t) => (
                  <button onClick={t} disabled={!bias.left.length} aria-label="Words that did not fit the Whisper prompt">
                    Whisper prompt ~{bias.fit}/{wordsList.length} fit
                  </button>
                )}
              >
                {() => (
                  <div style={{ padding: "6px 10px", maxWidth: 280 }}>
                    <div className="label" style={{ marginBottom: 6 }}>Did not fit, by usage</div>
                    {bias.left.map((w) => (
                      <div key={w.id} className="small" style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
                        <span>{w.text}</span>
                        <span className="mono">{w.uses}×</span>
                      </div>
                    ))}
                  </div>
                )}
              </Popover>
              <span>·</span>
              <span>ElevenLabs keyterms {settings.stt.elevenlabs.send_keyterms ? Math.min(wordsList.length, settings.stt.elevenlabs.max_keyterms) : "off"}</span>
              <span>·</span>
              <span>LLM hints {settings.cleanup.level === "none" ? "off" : "on"}</span>
            </div>
          )}
        </>
      )}

      {tab === "corrections" && (
        <>
          <Label>Corrections - what it hears, what it writes</Label>
          <div className="card">
            <AddCorrection
              inputRef={addRef}
              check={check}
              onAdd={async (heard, write) => {
                const target = wordsList.find((w) => norm(w.text) === norm(write));
                return upsert(target ? { ...input(target), aliases: [...target.aliases, heard.trim()] } : fresh("word", write, [heard]), `c${norm(heard)}`);
              }}
            />
            {!corrections.length ? (
              <Empty line={S.empty.corrections.line} help={S.empty.corrections.help} />
            ) : !shownCorrections.length ? (
              noMatch
            ) : (
              <div className="rows">
                {shownCorrections.map(({ alias, entry }) => (
                  <EditableRow
                    key={`${entry.id}-${alias}`}
                    lit={lit === `c${norm(alias)}`}
                    value={alias}
                    mono
                    check={(v) => (norm(v) === norm(alias) ? null : check(v))}
                    onSave={(v) => upsert({ ...input(entry), aliases: entry.aliases.map((a) => (a === alias ? v : a)) })}
                    after={
                      <>
                        <ArrowRight size={14} strokeWidth={1.5} className="to" />
                        <span className="ellipsis" style={{ flex: 1 }}>{entry.text}</span>
                      </>
                    }
                    meta={`used ${entry.uses}×`}
                    onDelete={() => removeAlias(entry, alias)}
                  />
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {tab === "snippets" && (
        <>
          <Label
            right={
              <button className="btn secondary sm" onClick={() => setSheet("new")}>
                <Plus size={14} strokeWidth={1.5} /> New snippet
              </button>
            }
          >
            Snippets - say the trigger, paste the text
          </Label>
          <div className="card">
            {!snippets.length ? (
              <Empty line={S.empty.snippets.line} help={S.empty.snippets.help} action={<button className="btn secondary" onClick={() => setSheet("new")}>New snippet</button>} />
            ) : !shownSnippets.length ? (
              noMatch
            ) : (
              <>
                <div className="cols-head">
                  <span className="label" style={{ width: "30%" }}>Trigger</span>
                  <span className="label">Expansion</span>
                </div>
                <div className="rows">
                  {shownSnippets.map((s) => (
                    <div key={s.id} className={`row hoverable d-row snip-row${lit === `s${norm(s.text)}` ? " fresh" : ""}`} onClick={() => setSheet(s)}>
                      <span className="trig ellipsis">{s.text}</span>
                      <span className="exp ellipsis">{(s.expansion ?? "").replace(/\n/g, " ↵ ")}</span>
                      <span className="meta">used {s.uses}×</span>
                      <span className="actions" style={{ marginLeft: 0 }}>
                        <button className="icon-btn sm" aria-label={`Edit ${s.text}`} onClick={(e) => (e.stopPropagation(), setSheet(s))}>
                          <Pencil size={14} strokeWidth={1.5} />
                        </button>
                        <button className="icon-btn sm del" aria-label={`Delete ${s.text}`} onClick={(e) => (e.stopPropagation(), remove(s))}>
                          <Trash2 size={14} strokeWidth={1.5} />
                        </button>
                      </span>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        </>
      )}

      <SnippetSheet
        entry={sheet}
        onClose={() => setSheet(null)}
        check={check}
        onSave={async (e) => {
          const err = await upsert(e, `s${norm(e.text)}`);
          if (!err) setSheet(null);
          return err;
        }}
      />
    </>
  );
}

function AddWord({ inputRef, check, onAdd }: { inputRef: React.RefObject<HTMLInputElement | null>; check: (t: string) => string | null; onAdd: (t: string) => Promise<string | null> }) {
  const [v, setV] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const live = check(v);
  const submit = async () => {
    if (!v.trim() || live) return setErr(live);
    const e = await onAdd(v.trim());
    if (e) setErr(e);
    else setV("");
  };
  const shown = err ?? (live?.startsWith("Already") || live?.startsWith("Max") ? live : null);
  return (
    <div className="add-row">
      <div className="grow">
        <input ref={inputRef} className={`input${shown ? " bad" : ""}`} placeholder="Add a word or phrase…" value={v} onChange={(e) => (setV(e.target.value), setErr(null))} onKeyDown={(e) => e.key === "Enter" && submit()} aria-label="New word" aria-invalid={!!shown} />
        {shown && (
          <span className="field-error" role="alert">
            <AlertCircle size={13} strokeWidth={1.5} /> {shown}
          </span>
        )}
      </div>
      <button className="btn secondary" onClick={submit} disabled={!v.trim()}>
        Add <span className="dim">↵</span>
      </button>
    </div>
  );
}

function AddCorrection({ inputRef, check, onAdd }: { inputRef: React.RefObject<HTMLInputElement | null>; check: (t: string) => string | null; onAdd: (heard: string, write: string) => Promise<string | null> }) {
  const [heard, setHeard] = useState("");
  const [write, setWrite] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const live = check(heard);
  const submit = async () => {
    if (!heard.trim() || !write.trim()) return;
    if (live) return setErr(live);
    const e = await onAdd(heard, write);
    if (e) setErr(e);
    else (setHeard(""), setWrite(""), inputRef.current?.focus());
  };
  const shown = err ?? (live?.startsWith("Already") ? live : null);
  return (
    <>
      <div className="add-row" style={{ borderBottom: 0 }}>
        <div className="grow">
          <input ref={inputRef} className={`input mono corr-heard${shown ? " bad" : ""}`} placeholder="heard as…" value={heard} onChange={(e) => (setHeard(e.target.value), setErr(null))} onKeyDown={(e) => e.key === "Enter" && submit()} aria-label="Heard as" />
          {shown && (
            <span className="field-error" role="alert">
              <AlertCircle size={13} strokeWidth={1.5} /> {shown}
            </span>
          )}
        </div>
        <span className="arrow">
          <ArrowRight size={15} strokeWidth={1.5} />
        </span>
        <div className="grow">
          <input className="input corr-write" placeholder="write as…" value={write} onChange={(e) => setWrite(e.target.value)} onKeyDown={(e) => e.key === "Enter" && submit()} aria-label="Write as" />
        </div>
        <button className="btn secondary" onClick={submit} disabled={!heard.trim() || !write.trim()}>
          Add <span className="dim">↵</span>
        </button>
      </div>
      <p className="preview-line" style={{ borderBottom: "1px solid var(--line)", paddingBottom: 12, margin: 0 }}>
        {heard.trim() && write.trim() ? (
          <>
            <span className="mono">&ldquo;{heard.trim()} is great&rdquo;</span> &rarr; &ldquo;<b>{write.trim()}</b> is great&rdquo;
          </>
        ) : (
          "Preview: type both sides to see the fix."
        )}
      </p>
    </>
  );
}

function EditableRow(p: { value: string; lit: boolean; mono?: boolean; check: (v: string) => string | null; onSave: (v: string) => Promise<string | null>; onDelete: () => void; meta: string; lead?: React.ReactNode; after?: React.ReactNode }) {
  const [edit, setEdit] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const save = async () => {
    if (edit === null) return;
    const v = edit.trim();
    if (v === p.value) return setEdit(null);
    const e = p.check(v) ?? (await p.onSave(v));
    if (e) setErr(e);
    else setEdit(null);
  };
  return (
    <div className={`row hoverable d-row${p.lit ? " fresh" : ""}`}>
      {p.lead}
      <div className="aliasline">
        {edit !== null ? (
          <div style={{ display: "flex", flexDirection: "column", flex: 1, padding: "6px 0" }}>
            <input
              autoFocus
              className={`input${p.mono ? " mono" : ""}${err ? " bad" : ""}`}
              value={edit}
              onChange={(e) => (setEdit(e.target.value), setErr(null))}
              onKeyDown={(e) => {
                if (e.key === "Enter") save();
                if (e.key === "Escape") (setEdit(null), setErr(null));
              }}
              onBlur={save}
              aria-label="Edit entry"
            />
            {err && <span className="field-error">{err}</span>}
          </div>
        ) : (
          <span className={`${p.mono ? "heard" : "term"} ellipsis selectable`}>{p.value}</span>
        )}
        {p.after}
      </div>
      <span className="meta">{p.meta}</span>
      <span className="actions" style={{ marginLeft: 0 }}>
        <button className="icon-btn sm" aria-label={`Edit ${p.value}`} onClick={() => setEdit(p.value)}>
          <Pencil size={14} strokeWidth={1.5} />
        </button>
        <button className="icon-btn sm del" aria-label={`Delete ${p.value}`} onClick={p.onDelete}>
          <Trash2 size={14} strokeWidth={1.5} />
        </button>
      </span>
    </div>
  );
}

const VARS = ["{date}", "{time}", "{weekday}"];
/** Same output as Rust expand_vars (dictionary.rs): %Y-%m-%d, %H:%M, English %A, local time. */
const expand = (s: string) => {
  const d = new Date();
  const p2 = (n: number) => String(n).padStart(2, "0");
  return s
    .split("{date}").join(`${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`)
    .split("{time}").join(`${p2(d.getHours())}:${p2(d.getMinutes())}`)
    .split("{weekday}").join(d.toLocaleDateString("en-US", { weekday: "long" }));
};

function SnippetSheet({ entry, onClose, check, onSave }: { entry: DictEntry | "new" | null; onClose: () => void; check: (t: string, ignore?: number) => string | null; onSave: (e: DictEntryInput) => Promise<string | null> }) {
  const [trigger, setTrigger] = useState("");
  const [exp, setExp] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const id = entry && entry !== "new" ? entry.id : undefined;
  useEffect(() => {
    if (!entry) return;
    setTrigger(entry === "new" ? "" : entry.text);
    setExp(entry === "new" ? "" : (entry.expansion ?? ""));
    setErr(null);
  }, [entry]);
  const insert = (v: string) => {
    const a = area.current!;
    const next = exp.slice(0, a.selectionStart) + v + exp.slice(a.selectionEnd);
    const at = a.selectionStart + v.length;
    setExp(next.slice(0, 4000));
    requestAnimationFrame(() => (a.focus(), a.setSelectionRange(at, at)));
  };
  const live = check(trigger, id);
  const save = async () => {
    if (live) return setErr(live);
    setErr(await onSave({ id: id ?? null, kind: "snippet", text: trigger.trim(), aliases: [], expansion: exp, exact_only: false, starred: entry !== "new" && !!entry?.starred }));
  };
  return (
    <Sheet open={!!entry} onClose={onClose} title={entry === "new" ? "New snippet" : "Edit snippet"}>
      <div className="field">
        <span className="label">Trigger - what you say</span>
        <input className={`input${err ? " bad" : ""}`} value={trigger} onChange={(e) => (setTrigger(e.target.value), setErr(null))} placeholder="my calendly" autoFocus />
        {(err || (live && trigger.trim())) && <span className="field-error">{err ?? live}</span>}
      </div>
      <div className="field">
        <span className="label">Expansion - what gets pasted</span>
        <textarea ref={area} className="input mono" rows={5} maxLength={4000} value={exp} onChange={(e) => setExp(e.target.value)} placeholder="https://calendly.com/you/30min" />
        <div className="field-foot">
          <div className="chips">
            {VARS.map((v) => (
              <button key={v} className="chip" onClick={() => insert(v)}>
                {v}
              </button>
            ))}
          </div>
          <span className="mono small">
            {exp.length} / 4000
          </span>
        </div>
      </div>
      <div className="field">
        <span className="label">Preview</span>
        <div className="preview-box selectable">{exp ? expand(exp) : <span className="dim">The expansion shows here, with variables filled in.</span>}</div>
      </div>
      <div className="sheet-foot">
        <button className="btn ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="btn primary" onClick={save} disabled={!trigger.trim() || !exp.trim()}>
          Save snippet
        </button>
      </div>
    </Sheet>
  );
}
