#!/usr/bin/env python3
"""LoCoMo (snap-research/locomo) evaluation harness for memoricai (stdlib only).

Phases: ingest | wait | retrieve | answer | judge | report
Reader: gpt-4o-mini (matches mem0's published generator); judge: gpt-4o-2024-08-06.
Contexts: memoricai hybrid (digest + top sessions, aggregation-aware) — the
same policy as scripts/longmemeval.py.

Setup:
  1. Clone https://github.com/snap-research/locomo (or set LOCOMO_DATA to
     locomo10.json).
  2. Start `memoricai serve` on a fresh database; put its API key in
     `locomo.key` next to this script (or LOCOMO_API_KEY).
  3. Put an OpenAI key in `openai.key` next to this script.

Env: LOCOMO_DATA, LOCOMO_API_KEY, LOCOMO_BASE, LOCOMO_SPECULATE=1 (re-answer
with a speculation-permitting prompt for the mem0-comparable categories 1-4
cut; the default abstaining prompt is right for the adversarial category).
"""
import ast
import concurrent.futures as cf
import json
import os
import pathlib
import sys
import time
import urllib.error
import urllib.request

SCRATCH = pathlib.Path(__file__).parent
BASE = os.environ.get("LOCOMO_BASE", "http://127.0.0.1:6767")
KEY = os.environ.get("LOCOMO_API_KEY") or (
    (SCRATCH / "locomo.key").read_text().strip() if (SCRATCH / "locomo.key").exists() else ""
)
OPENAI_KEY = (SCRATCH / "openai.key").read_text().strip()
DATA = pathlib.Path(os.environ.get("LOCOMO_DATA", SCRATCH / "locomo" / "data" / "locomo10.json"))
STATE = SCRATCH / "locomo_state.json"
ROWS = SCRATCH / ("locomo_rows2.jsonl" if os.environ.get("LOCOMO_SPECULATE") == "1" else "locomo_rows.jsonl")
READER_MODEL = "gpt-4o-mini"
JUDGE_MODEL = "gpt-4o-2024-08-06"
CAT_NAMES = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop", 5: "adversarial"}

AGG_PHRASES = (
    "how many", "how much", "how often", "count", "list all", "what are all",
    "what are the", "which of", "all the", "in total",
)


def is_agg(q):
    q = q.lower()
    return any(p in q for p in AGG_PHRASES)


def call(method, path, body=None, timeout=120):
    req = urllib.request.Request(
        BASE + path, data=json.dumps(body).encode() if body is not None else None, method=method
    )
    req.add_header("Authorization", "Bearer " + KEY)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


def openai_call(model, prompt, max_tokens=400, timeout=120):
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0,
        "max_tokens": max_tokens,
    }
    for attempt in range(6):
        req = urllib.request.Request(
            "https://api.openai.com/v1/chat/completions",
            data=json.dumps(body).encode(),
            method="POST",
        )
        req.add_header("Authorization", "Bearer " + OPENAI_KEY)
        req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.loads(r.read().decode())["choices"][0]["message"]["content"].strip()
        except urllib.error.HTTPError as e:
            if e.code in (429, 500, 502, 503) and attempt < 5:
                time.sleep(2 ** attempt)
                continue
            raise
    raise RuntimeError("retries exhausted")


def load_samples():
    return json.load(open(DATA))


def session_items(conv):
    """Yield (session_no, date_time, turns) in order."""
    n = 1
    while f"session_{n}" in conv:
        yield n, conv.get(f"session_{n}_date_time", "?"), conv[f"session_{n}"]
        n += 1


def render_session(conv, no, date, turns):
    lines = [f"Chat session between {conv['speaker_a']} and {conv['speaker_b']} on {date}."]
    for t in turns:
        text = t.get("text", "")
        cap = t.get("blip_caption")
        if cap:
            text = f"{text} [shared a photo: {cap}]"
        lines.append(f"{t['speaker']}: {text}")
    return "\n".join(lines)


def cmd_ingest():
    samples = load_samples()
    state = {"docs": []}
    jobs = []
    for s in samples:
        tag = f"mc_locomo_{s['sample_id']}"
        for no, date, turns in session_items(s["conversation"]):
            jobs.append((tag, no, date, render_session(s["conversation"], no, date, turns)))

    def enqueue(job):
        tag, no, date, content = job
        code, body = call(
            "POST", "/v1/documents",
            {"content": content, "containerTag": tag,
             "metadata": {"session": str(no), "date": date}},
        )
        return {"doc_id": body.get("id"), "tag": tag, "session": str(no)} if code == 200 else None

    with cf.ThreadPoolExecutor(12) as ex:
        for r in ex.map(enqueue, jobs):
            if r:
                state["docs"].append(r)
    STATE.write_text(json.dumps(state))
    print(f"enqueued {len(state['docs'])}/{len(jobs)} session docs across {len(samples)} conversations")


def cmd_wait():
    state = json.loads(STATE.read_text())
    pending = {d["doc_id"] for d in state["docs"]}
    t0 = time.time()
    while pending and time.time() - t0 < 1800:
        done = set()
        with cf.ThreadPoolExecutor(16) as ex:
            futs = {ex.submit(call, "GET", f"/v1/documents/{d}"): d for d in pending}
            for f in cf.as_completed(futs):
                _, b = f.result()
                if b.get("status") in ("done", "failed"):
                    done.add(futs[f])
                    if b.get("status") == "failed":
                        print("FAILED:", futs[f])
        pending -= done
        print(f"t={time.time()-t0:.0f}s pending={len(pending)}", flush=True)
        if pending:
            time.sleep(5)


def evidence_sessions(ev):
    """evidence like "['D1:3', 'D2:5']" -> {'1','2'} (session numbers)."""
    if isinstance(ev, str):
        try:
            ev = ast.literal_eval(ev)
        except Exception:
            ev = []
    out = set()
    for d in ev or []:
        d = str(d)
        if d.startswith("D") and ":" in d:
            out.add(d[1:].split(":")[0])
    return out


def cmd_retrieve():
    samples = load_samples()
    tasks = []
    for s in samples:
        tag = f"mc_locomo_{s['sample_id']}"
        for i, q in enumerate(s["qa"]):
            tasks.append((tag, s["sample_id"], i, q))

    def one(task):
        tag, sid, i, q = task
        question = q["question"]
        agg = is_agg(question)
        _, dbody = call("POST", "/v1/search",
                        {"q": question, "containerTag": tag, "searchMode": "memories",
                         "limit": 20, "threshold": 0.05, "digest": True})
        _, body = call("POST", "/v1/documents/search",
                       {"q": question, "containerTags": [tag], "limit": 30,
                        "chunkThreshold": 0.05, "documentThreshold": 0.05,
                        "includeFullDocs": True})
        digest = dbody.get("digest") or ""
        max_sessions = 8 if agg else 4
        retrieved, sessions, seen = [], [], set()
        for r in body.get("results", []):
            meta = r.get("metadata") or {}
            sess = meta.get("session") or "?"
            if sess in seen:
                continue
            seen.add(sess)
            retrieved.append(sess)
            if len(sessions) < max_sessions and r.get("content"):
                sessions.append(f"## Session on {meta.get('date','?')}\n{r['content']}")
        context = (digest + "\n\nMost relevant sessions:\n\n" + "\n\n".join(sessions))[:60000]
        evid = evidence_sessions(q.get("evidence"))
        cat = int(q["category"])
        return {
            "sample_id": sid, "qa_index": i, "category": cat,
            "question": question,
            "answer": q.get("answer", ""),
            "adversarial_answer": q.get("adversarial_answer", ""),
            "evidence_sessions": sorted(evid),
            "retrieved_sessions": retrieved,
            "r_any_at1": bool(evid & set(retrieved[:1])) if evid else None,
            "r_any_at5": bool(evid & set(retrieved[:5])) if evid else None,
            "r_any_at10": bool(evid & set(retrieved[:10])) if evid else None,
            "cov_all_at10": (evid <= set(retrieved[:10])) if evid else None,
            "context": context,
        }

    rows = []
    with cf.ThreadPoolExecutor(8) as ex:
        for n, r in enumerate(ex.map(one, tasks)):
            rows.append(r)
            if n % 200 == 0:
                print(f"retrieved {n}/{len(tasks)}", flush=True)
    ROWS.write_text("\n".join(json.dumps(r) for r in rows))
    print(f"retrieve done: {len(rows)} questions")


SPECULATE = os.environ.get("LOCOMO_SPECULATE") == "1"

ANSWER_TMPL = (
    "Based on the following context from past conversations, answer the question. "
    "Be brief and specific — give the answer directly, with dates in the format "
    "they appear in the conversation. For speculative or hypothetical questions "
    "(\"would she likely...\"), reason from what the context reveals about the "
    "speakers and give your best answer.\n\n"
    "Context:\n{}\n\nQuestion: {}\nAnswer:"
) if SPECULATE else (
    "Based on the following context from past conversations, answer the question. "
    "Be brief and specific — give the answer directly, with dates in the format "
    "they appear in the conversation. If the context does not contain the answer, "
    "say \"No information available\".\n\n"
    "Context:\n{}\n\nQuestion: {}\nAnswer:"
)

JUDGE_TMPL = "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no. Do not penalize date-format differences when the date is the same.\n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."

JUDGE_ADV_TMPL = "I will give you a question that cannot be answered from the underlying conversation, and a response from a model. Please answer yes if the model correctly indicates that the information is not available, not mentioned, or that it cannot answer. Answer no if the model asserts a specific answer.\n\nQuestion: {}\n\nModel Response: {}\n\nDoes the model correctly indicate the information is unavailable? Answer yes or no only."


def cmd_answer():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]

    def do(r):
        try:
            r["hypothesis"] = openai_call(READER_MODEL, ANSWER_TMPL.format(r["context"], r["question"]))
        except Exception as e:
            r["hypothesis"] = ""
            r["answer_error"] = str(e)
        return r

    out = []
    with cf.ThreadPoolExecutor(8) as ex:
        for n, r in enumerate(ex.map(do, rows)):
            out.append(r)
            if n % 200 == 0:
                print(f"answered {n}/{len(rows)}", flush=True)
    ROWS.write_text("\n".join(json.dumps(r) for r in out))


def cmd_judge():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]

    def do(r):
        try:
            if r["category"] == 5:
                prompt = JUDGE_ADV_TMPL.format(r["question"], r["hypothesis"])
            else:
                prompt = JUDGE_TMPL.format(r["question"], r["answer"], r["hypothesis"])
            resp = openai_call(JUDGE_MODEL, prompt, max_tokens=10)
            r["judge_raw"] = resp[:60]
            r["correct"] = "yes" in resp.lower()[:20]
        except Exception as e:
            r["judge_raw"] = f"error: {e}"
            r["correct"] = False
        return r

    out = []
    with cf.ThreadPoolExecutor(8) as ex:
        for n, r in enumerate(ex.map(do, rows)):
            out.append(r)
            if n % 200 == 0:
                print(f"judged {n}/{len(rows)}", flush=True)
    ROWS.write_text("\n".join(json.dumps(r) for r in out))


def cmd_report():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]
    by_cat = {}
    for r in rows:
        by_cat.setdefault(r["category"], []).append(r)
    out = {"n": len(rows)}
    out["overall_all_categories"] = round(sum(r["correct"] for r in rows) / len(rows) * 100, 1)
    non_adv = [r for r in rows if r["category"] != 5]
    out["overall_excl_adversarial"] = round(sum(r["correct"] for r in non_adv) / len(non_adv) * 100, 1)
    out["by_category"] = {
        f"{c}:{CAT_NAMES.get(c, '?')}": {
            "n": len(rs),
            "acc": round(sum(r["correct"] for r in rs) / len(rs) * 100, 1),
        }
        for c, rs in sorted(by_cat.items())
    }
    with_ev = [r for r in rows if r["r_any_at10"] is not None]
    for k in ("r_any_at1", "r_any_at5", "r_any_at10", "cov_all_at10"):
        out[k] = round(sum(bool(r[k]) for r in with_ev) / len(with_ev) * 100, 1)
    out["n_with_evidence"] = len(with_ev)
    errs = sum(1 for r in rows if r.get("answer_error"))
    if errs:
        out["answer_errors"] = errs
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    {
        "ingest": cmd_ingest,
        "wait": cmd_wait,
        "retrieve": cmd_retrieve,
        "answer": cmd_answer,
        "judge": cmd_judge,
        "report": cmd_report,
    }[sys.argv[1]]()
