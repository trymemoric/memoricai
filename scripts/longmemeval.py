#!/usr/bin/env python3
"""LongMemEval-S evaluation harness for memoricai (stdlib only).

Phases (run separately via subcommand):
  ingest   - stratified sample (LME_N questions), enqueue haystack sessions
  wait     - poll until all docs are done
  retrieve - build answer contexts + session-recall metrics
  answer   - generate answers with the official templates
  judge    - judge with the official LongMemEval prompts
  report   - aggregate metrics

Setup:
  1. Download the dataset (https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned,
     `longmemeval_s_cleaned.json`) and point LME_DATA at it.
  2. Start `memoricai serve` on a fresh database; put its API key in
     `lme.key` next to this script (or LME_API_KEY).
  3. For the reader/judge, set OPENAI_API_KEY (or put it in `openai.key` next
     to this script) and set LME_READER=openai (default is `claude -p`).

Env: LME_DATA, LME_API_KEY, LME_N (default 100; 500 = full set), LME_SUFFIX,
LME_READER=claude|openai, LME_WHOLE_SESSIONS=1, LME_MERGE_STATE,
LME_DIGEST=1 (digest-only contexts), LME_HYBRID=1 (digest + top sessions —
the v0.3.0 reported configuration), LME_CONTEXT=1 (bounded `/v1/context`
configuration). State lives in lme_state*.json / lme_rows*.jsonl next to this
script.
"""
import concurrent.futures as cf
import json
import os
import pathlib
import random
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

SCRATCH = pathlib.Path(__file__).parent
BASE = os.environ.get("LME_BASE", "http://127.0.0.1:7373")
KEY = os.environ.get("LME_API_KEY") or (
    (SCRATCH / "lme.key").read_text().strip() if (SCRATCH / "lme.key").exists() else ""
)
DATA = pathlib.Path(
    os.environ.get("LME_DATA", SCRATCH / "longmemeval" / "data" / "longmemeval_s_cleaned.json")
)
SUFFIX = os.environ.get("LME_SUFFIX", "")
STATE = SCRATCH / f"lme_state{SUFFIX}.json"
ROWS = SCRATCH / f"lme_rows{SUFFIX}.jsonl"
N_SAMPLE = int(os.environ.get("LME_N", "100"))
SEED = 42
TOP_K = 10
CTX_CHAR_CAP = 40000
CLAUDE_MODEL = "sonnet"
READER = os.environ.get("LME_READER", "claude")  # claude | openai
# Reader model (the system under test). Override to re-evaluate a cheaper model.
OPENAI_MODEL = os.environ.get("LME_OPENAI_MODEL", "gpt-4o-2024-08-06")
# Judge stays fixed across reader-model runs so accuracy stays comparable to the
# baseline; only the reader (LME_OPENAI_MODEL) should vary between runs.
JUDGE_MODEL = os.environ.get("LME_JUDGE_MODEL", "gpt-4o-2024-08-06")
OPENAI_KEY = os.environ.get("OPENAI_API_KEY") or (
    (SCRATCH / "openai.key").read_text().strip()
    if (SCRATCH / "openai.key").exists()
    else ""
)
# real extractors return few facts per doc, so whole sessions fit in one doc;
# mock sentence-splitting needs small parts to stay under the 100-fact cap
WHOLE_SESSIONS = os.environ.get("LME_WHOLE_SESSIONS") == "1"
# context granularity: matching chunks (default) vs full retrieved sessions
# (official LongMemEval session-granularity protocol)
FULL_DOCS = os.environ.get("LME_FULL_DOCS") == "1"
if FULL_DOCS:
    CTX_CHAR_CAP = 120000


def call(method, path, body=None, timeout=120):
    req = urllib.request.Request(
        BASE + path,
        data=json.dumps(body).encode() if body is not None else None,
        method=method,
    )
    req.add_header("Authorization", "Bearer " + KEY)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            code, payload = r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        code, payload = e.code, e.read().decode()
    ms = (time.perf_counter() - t0) * 1000
    try:
        return code, json.loads(payload) if payload else {}, ms
    except Exception:
        return code, {"_raw": payload[:300]}, ms


def sample_questions():
    data = json.load(open(DATA))
    by_type = {}
    for q in data:
        by_type.setdefault(q["question_type"], []).append(q)
    total = len(data)
    rng = random.Random(SEED)
    picked = []
    # proportional allocation, largest-remainder
    alloc = {}
    rema = []
    used = 0
    for t, qs in sorted(by_type.items()):
        exact = len(qs) / total * N_SAMPLE
        alloc[t] = int(exact)
        used += int(exact)
        rema.append((exact - int(exact), t))
    for _, t in sorted(rema, reverse=True)[: N_SAMPLE - used]:
        alloc[t] += 1
    for t, qs in sorted(by_type.items()):
        picked.extend(rng.sample(qs, alloc[t]))
    return picked


def tag_for(qid):
    return "mc_lme_" + re.sub(r"[^a-zA-Z0-9_]", "_", qid)


PART_CHAR_CAP = 80000 if WHOLE_SESSIONS else 3500


def render_session_parts(date, turns):
    """Split a session into sub-documents under PART_CHAR_CAP chars each.

    The mock extractor sentence-splits whole documents and the engine rejects
    >100 facts per doc, so parts must stay small. Giant single turns are
    hard-split.
    """
    pieces = []
    for t in turns:
        role = "User" if t["role"] == "user" else "Assistant"
        text = f"{role}: {t['content']}"
        while len(text) > PART_CHAR_CAP:
            pieces.append(text[:PART_CHAR_CAP])
            text = f"{role} (cont.): " + text[PART_CHAR_CAP:]
        pieces.append(text)
    parts = []
    cur = []
    cur_len = 0
    for p in pieces:
        if cur and cur_len + len(p) > PART_CHAR_CAP:
            parts.append(cur)
            cur, cur_len = [], 0
        cur.append(p)
        cur_len += len(p)
    if cur:
        parts.append(cur)
    header = f"Chat session recorded on {date}"
    return [
        f"{header}" + (f" (part {i+1}/{len(parts)})." if len(parts) > 1 else ".") + "\n\n" + "\n\n".join(chunk)
        for i, chunk in enumerate(parts)
    ]


def cmd_ingest():
    qs = sample_questions()
    # LME_MERGE_STATE: reuse questions/docs already ingested under a prior
    # state file (same DB) and only enqueue the rest.
    done_qids = set()
    merged_questions, merged_docs = [], []
    merge = os.environ.get("LME_MERGE_STATE")
    if merge:
        prior = json.loads((SCRATCH / merge).read_text())
        done_qids = {q["question_id"] for q in prior["questions"]}
        merged_questions = prior["questions"]
        merged_docs = prior["docs"]
        print(f"merging {len(done_qids)} already-ingested questions from {merge}")
    state = {"questions": merged_questions, "docs": merged_docs}
    jobs = []
    qs = [q for q in qs if q["question_id"] not in done_qids]
    for q in qs:
        tag = tag_for(q["question_id"])
        state["questions"].append(
            {
                "question_id": q["question_id"],
                "question_type": q["question_type"],
                "question": q["question"],
                "question_date": q["question_date"],
                "answer": q["answer"],
                "answer_session_ids": q["answer_session_ids"],
                "tag": tag,
                "n_sessions": len(q["haystack_sessions"]),
            }
        )
        for sid, date, turns in zip(
            q["haystack_session_ids"], q["haystack_dates"], q["haystack_sessions"]
        ):
            for part in render_session_parts(date, turns):
                jobs.append((tag, sid, date, part))

    print(f"{len(qs)} questions, {len(jobs)} session-docs to ingest")
    failures = 0

    def enqueue(job):
        tag, sid, date, content = job
        code, body, _ = call(
            "POST",
            "/v1/documents",
            {
                "content": content,
                "containerTag": tag,
                "metadata": {"sessionId": sid, "date": date},
            },
        )
        if code != 200:
            return None
        return {"doc_id": body.get("id"), "tag": tag, "session_id": sid, "date": date}

    t0 = time.time()
    with cf.ThreadPoolExecutor(12) as ex:
        for res in ex.map(enqueue, jobs):
            if res is None:
                failures += 1
            else:
                state["docs"].append(res)
    print(f"enqueued {len(state['docs'])} docs, {failures} failures, {time.time()-t0:.0f}s")
    STATE.write_text(json.dumps(state))


def cmd_wait():
    state = json.loads(STATE.read_text())
    ids = [d["doc_id"] for d in state["docs"]]
    pending = set(ids)
    deadline = time.time() + 3600
    t0 = time.time()
    while pending and time.time() < deadline:
        done_now = set()
        failed = 0
        with cf.ThreadPoolExecutor(16) as ex:
            futs = {ex.submit(call, "GET", f"/v1/documents/{d}"): d for d in list(pending)}
            for f in cf.as_completed(futs):
                c, b, _ = f.result()
                s = b.get("status")
                if s == "done":
                    done_now.add(futs[f])
                elif s == "failed":
                    done_now.add(futs[f])
                    failed += 1
        pending -= done_now
        print(f"t={time.time()-t0:.0f}s remaining={len(pending)} (failed this sweep: {failed})", flush=True)
        if pending:
            time.sleep(5)
    print(f"pipeline drained in {time.time()-t0:.0f}s, unfinished={len(pending)}")


DIGEST_MODE = os.environ.get("LME_DIGEST") == "1"
HYBRID_MODE = os.environ.get("LME_HYBRID") == "1"
CONTEXT_MODE = os.environ.get("LME_CONTEXT") == "1"


def cmd_retrieve():
    state = json.loads(STATE.read_text())
    doc2sess = {d["doc_id"]: d["session_id"] for d in state["docs"]}
    rows = []
    lat = []
    if CONTEXT_MODE:
        # Same ~11k-token answer-context budget and official downstream prompts
        # as the reported hybrid run, assembled by the engine's bounded packer.
        for q in state["questions"]:
            code, body, ms = call(
                "POST",
                "/v1/context",
                {
                    "q": q["question"],
                    "containerTag": q["tag"],
                    "mode": "auto",
                    "budgetTokens": 11250,
                    "maxSources": TOP_K,
                    "threshold": 0.05,
                    "includeDigest": True,
                },
            )
            lat.append(ms)
            evidence = body.get("evidence", []) if code == 200 else []
            diagnostics = body.get("diagnostics", {}) if code == 200 else {}
            retrieved = []
            included = []
            seen_retrieved = set()
            seen_included = set()
            for item in evidence:
                sid = item.get("sessionId") or item.get("sourceId") or "?"
                if sid not in seen_retrieved:
                    seen_retrieved.add(sid)
                    retrieved.append(sid)
                if item.get("included") and sid not in seen_included:
                    seen_included.add(sid)
                    included.append(sid)
            evid = set(q["answer_session_ids"])
            context = body.get("context", "") if code == 200 else ""
            rows.append(
                {
                    **q,
                    "retrieved_sessions": retrieved,
                    "included_sessions": included,
                    "recall_any_at5": bool(evid & set(retrieved[:5])),
                    "recall_any_at10": bool(evid & set(retrieved[:10])),
                    "coverage_all_at10": evid <= set(retrieved[:10]) if evid else True,
                    "context_recall_any": bool(evid & set(included)),
                    "context_coverage_all": evid <= set(included) if evid else True,
                    "search_ms": round(ms, 1),
                    "context": context,
                    "context_diagnostics": diagnostics,
                }
            )
            print(
                f"{q['question_id']} context={len(context)}ch "
                f"sources={len(included)} r@10={bool(evid & set(retrieved[:10]))} "
                f"{ms:.0f}ms",
                flush=True,
            )
        ROWS.write_text("\n".join(json.dumps(r) for r in rows))
        import statistics

        print(
            f"context retrieve done; latency mean={statistics.fmean(lat):.0f}ms; "
            f"R_any@5={sum(r['recall_any_at5'] for r in rows)}/{len(rows)} "
            f"R_any@10={sum(r['recall_any_at10'] for r in rows)}/{len(rows)} "
            f"Cov_all@10={sum(r['coverage_all_at10'] for r in rows)}/{len(rows)} "
            f"Context_cov={sum(r['context_coverage_all'] for r in rows)}/{len(rows)}"
        )
        return
    if HYBRID_MODE:
        # digest (aggregated facts) + top full sessions, ~10k tokens total
        for q in state["questions"]:
            _, dbody, dms = call(
                "POST",
                "/v1/search",
                {
                    "q": q["question"],
                    "containerTag": q["tag"],
                    "searchMode": "memories",
                    "limit": 20,
                    "threshold": 0.05,
                    "digest": True,
                },
            )
            code, body, ms = call(
                "POST",
                "/v1/documents/search",
                {
                    "q": q["question"],
                    "containerTags": [q["tag"]],
                    "limit": 30,
                    "chunkThreshold": 0.05,
                    "documentThreshold": 0.05,
                    "includeFullDocs": True,
                },
            )
            lat.append(dms + ms)
            digest = dbody.get("digest") or ""
            results = body.get("results", []) if code == 200 else []
            retrieved, sessions, seen = [], [], set()
            for r in results:
                meta = r.get("metadata") or {}
                sid = meta.get("sessionId") or "?"
                if sid in seen:
                    continue
                seen.add(sid)
                retrieved.append(sid)
                if len(sessions) < 4 and r.get("content"):
                    sessions.append(
                        f"## Chat session on {meta.get('date','?')}\n{r['content']}"
                    )
            context = (
                digest
                + "\n\nMost relevant chat sessions:\n\n"
                + "\n\n".join(sessions)
            )[:45000]
            evid = set(q["answer_session_ids"])
            rows.append(
                {
                    **q,
                    "retrieved_sessions": retrieved,
                    "recall_any_at5": bool(evid & set(retrieved[:5])),
                    "recall_any_at10": bool(evid & set(retrieved[:10])),
                    "coverage_all_at10": evid <= set(retrieved[:10]) if evid else True,
                    "search_ms": round(dms + ms, 1),
                    "context": context,
                }
            )
            print(f"{q['question_id']} hybrid={len(context)}ch {dms+ms:.0f}ms", flush=True)
        ROWS.write_text("\n".join(json.dumps(r) for r in rows))
        import statistics

        print(f"hybrid retrieve done; latency mean={statistics.fmean(lat):.0f}ms")
        return
    if DIGEST_MODE:
        for q in state["questions"]:
            code, body, ms = call(
                "POST",
                "/v1/search",
                {
                    "q": q["question"],
                    "containerTag": q["tag"],
                    "searchMode": "memories",
                    "limit": 20,
                    "threshold": 0.05,
                    "digest": True,
                },
            )
            lat.append(ms)
            context = (body.get("digest") or "") if code == 200 else ""
            rows.append(
                {
                    **q,
                    "retrieved_sessions": [],
                    "recall_any_at5": False,
                    "recall_any_at10": False,
                    "coverage_all_at10": False,
                    "search_ms": round(ms, 1),
                    "context": context,
                }
            )
            print(f"{q['question_id']} digest={len(context)}ch {ms:.0f}ms", flush=True)
        ROWS.write_text("\n".join(json.dumps(r) for r in rows))
        import statistics

        print(f"digest retrieve done; search latency mean={statistics.fmean(lat):.0f}ms")
        return
    for q in state["questions"]:
        code, body, ms = call(
            "POST",
            "/v1/documents/search",
            {
                "q": q["question"],
                "containerTags": [q["tag"]],
                "limit": 30,
                "chunkThreshold": 0.05,
                "documentThreshold": 0.05,
                "includeFullDocs": FULL_DOCS,
            },
        )
        lat.append(ms)
        results = body.get("results", []) if code == 200 else []
        # dedupe to unique sessions in rank order (sessions were split into
        # multiple sub-docs at ingest); top-K unique sessions form the context
        retrieved = []
        ctx_parts = []
        seen = set()
        for r in results:
            did = r.get("documentId")
            meta = r.get("metadata") or {}
            sid = meta.get("sessionId") or doc2sess.get(did, "?")
            date = meta.get("date", "?")
            if FULL_DOCS and r.get("content"):
                text = r["content"]
            else:
                chunks = [c.get("content", "") for c in r.get("chunks", [])]
                text = "\n...\n".join(chunks)
            if sid not in seen:
                seen.add(sid)
                retrieved.append(sid)
                if len(retrieved) <= TOP_K:
                    ctx_parts.append(f"## Chat session on {date}\n{text}")
            elif retrieved.index(sid) < TOP_K:
                ctx_parts.append(f"## Chat session on {date} (more)\n{text}")
        context = "\n\n".join(ctx_parts)[:CTX_CHAR_CAP]
        evid = set(q["answer_session_ids"])
        r5 = bool(evid & set(retrieved[:5]))
        r10 = bool(evid & set(retrieved[:10]))
        cov10 = evid <= set(retrieved[:10]) if evid else True
        rows.append(
            {
                **q,
                "retrieved_sessions": retrieved,
                "recall_any_at5": r5,
                "recall_any_at10": r10,
                "coverage_all_at10": cov10,
                "search_ms": round(ms, 1),
                "context": context,
            }
        )
        print(f"{q['question_id']} sessions={len(retrieved)} r@10={r10} {ms:.0f}ms", flush=True)
    ROWS.write_text("\n".join(json.dumps(r) for r in rows))
    import statistics

    print(
        f"search latency mean={statistics.fmean(lat):.0f}ms; "
        f"R_any@5={sum(r['recall_any_at5'] for r in rows)}/{len(rows)} "
        f"R_any@10={sum(r['recall_any_at10'] for r in rows)}/{len(rows)} "
        f"Cov_all@10={sum(r['coverage_all_at10'] for r in rows)}/{len(rows)}"
    )


ANSWER_PLAIN = (
    "I will give you several history chats between you and a user. "
    "Please answer the question based on the relevant chat history.\n\n\n"
    "History Chats:\n\n{}\n\nCurrent Date: {}\nQuestion: {}\nAnswer:"
)
ANSWER_COT = (
    "I will give you several history chats between you and a user. "
    "Please answer the question based on the relevant chat history. "
    "Answer the question step by step: first extract all the relevant information, "
    "and then reason over the information to get the answer.\n\n\n"
    "History Chats:\n\n{}\n\nCurrent Date: {}\nQuestion: {}\nAnswer (step by step):"
)


def claude_call(prompt, timeout=240):
    p = subprocess.run(
        ["claude", "-p", "--model", CLAUDE_MODEL, "--strict-mcp-config",
         "--mcp-config", '{"mcpServers":{}}'],
        input=prompt,
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=str(SCRATCH),
    )
    return p.stdout.strip()


def openai_call(prompt, max_tokens=1000, timeout=180, model=None):
    body = {
        "model": model or OPENAI_MODEL,
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
                d = json.loads(r.read().decode())
                return d["choices"][0]["message"]["content"].strip()
        except urllib.error.HTTPError as e:
            if e.code in (429, 500, 502, 503) and attempt < 5:
                time.sleep(2**attempt)
                continue
            raise
    raise RuntimeError("openai retries exhausted")


def reader_call(prompt, max_tokens=1000, timeout=240):
    if READER == "openai":
        return openai_call(prompt, max_tokens=max_tokens, timeout=timeout)
    return claude_call(prompt, timeout=timeout)


def judge_call(prompt, max_tokens=10, timeout=120):
    # Grade with the fixed JUDGE_MODEL so swapping the reader does not swap the grader.
    return openai_call(prompt, max_tokens=max_tokens, timeout=timeout, model=JUDGE_MODEL)


def cmd_answer():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]

    def do(row):
        tmpl = ANSWER_COT if row["question_type"] == "temporal-reasoning" else ANSWER_PLAIN
        prompt = tmpl.format(row["context"], row["question_date"], row["question"])
        try:
            row["hypothesis"] = reader_call(prompt)
        except Exception as e:
            row["hypothesis"] = ""
            row["answer_error"] = str(e)
        print(f"answered {row['question_id']} ({len(row.get('hypothesis',''))} chars)", flush=True)
        return row

    with cf.ThreadPoolExecutor(5) as ex:
        rows = list(ex.map(do, rows))
    ROWS.write_text("\n".join(json.dumps(r) for r in rows))


JUDGE_DEFAULT = "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no. \n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."
JUDGE_TEMPORAL = "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response is equivalent to the correct answer or contains all the intermediate steps to get the correct answer, you should also answer yes. If the response only contains a subset of the information required by the answer, answer no. In addition, do not penalize off-by-one errors for the number of days. If the question asks for the number of days/weeks/months, etc., and the model makes off-by-one errors (e.g., predicting 19 days when the answer is 18), the model's response is still correct. \n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."
JUDGE_KU = "I will give you a question, a correct answer, and a response from a model. Please answer yes if the response contains the correct answer. Otherwise, answer no. If the response contains some previous information along with an updated answer, the response should be considered as correct as long as the updated answer is the required answer.\n\nQuestion: {}\n\nCorrect Answer: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."
JUDGE_PREF = "I will give you a question, a rubric for desired personalized response, and a response from a model. Please answer yes if the response satisfies the desired response. Otherwise, answer no. The model does not need to reflect all the points in the rubric. The response is correct as long as it recalls and utilizes the user's personal information correctly.\n\nQuestion: {}\n\nRubric: {}\n\nModel Response: {}\n\nIs the model response correct? Answer yes or no only."
JUDGE_ABS = "I will give you an unanswerable question, an explanation, and a response from a model. Please answer yes if the model correctly identifies the question as unanswerable. The model could say that the information is incomplete, or some other information is given but the asked information is not.\n\nQuestion: {}\n\nExplanation: {}\n\nModel Response: {}\n\nDoes the model correctly identify the question as unanswerable? Answer yes or no only."


def judge_prompt(row):
    q, ans, hyp, t = row["question"], row["answer"], row["hypothesis"], row["question_type"]
    if "_abs" in row["question_id"]:
        return JUDGE_ABS.format(q, ans, hyp)
    if t == "temporal-reasoning":
        return JUDGE_TEMPORAL.format(q, ans, hyp)
    if t == "knowledge-update":
        return JUDGE_KU.format(q, ans, hyp)
    if t == "single-session-preference":
        return JUDGE_PREF.format(q, ans, hyp)
    return JUDGE_DEFAULT.format(q, ans, hyp)


def cmd_judge():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]

    def do(row):
        try:
            resp = judge_call(judge_prompt(row))
            row["judge_raw"] = resp[:100]
            row["correct"] = "yes" in resp.lower()[:20]
        except Exception as e:
            row["judge_raw"] = f"error: {e}"
            row["correct"] = False
        print(f"judged {row['question_id']}: {row['correct']}", flush=True)
        return row

    with cf.ThreadPoolExecutor(5) as ex:
        rows = list(ex.map(do, rows))
    ROWS.write_text("\n".join(json.dumps(r) for r in rows))


def cmd_report():
    rows = [json.loads(l) for l in ROWS.read_text().splitlines()]
    by_type = {}
    for r in rows:
        key = r["question_type"]
        by_type.setdefault(key, []).append(r)
    out = {"n": len(rows)}
    out["overall_acc"] = round(sum(r["correct"] for r in rows) / len(rows) * 100, 1)
    abst = [r for r in rows if "_abs" in r["question_id"]]
    nonabst = [r for r in rows if "_abs" not in r["question_id"]]
    if abst:
        out["abstention_acc"] = round(sum(r["correct"] for r in abst) / len(abst) * 100, 1)
        out["abstention_n"] = len(abst)
    out["nonabstention_acc"] = round(
        sum(r["correct"] for r in nonabst) / len(nonabst) * 100, 1
    )
    out["by_type"] = {
        t: {"n": len(rs), "acc": round(sum(r["correct"] for r in rs) / len(rs) * 100, 1)}
        for t, rs in sorted(by_type.items())
    }
    out["recall_any_at5"] = round(sum(r["recall_any_at5"] for r in rows) / len(rows) * 100, 1)
    out["recall_any_at10"] = round(sum(r["recall_any_at10"] for r in rows) / len(rows) * 100, 1)
    out["coverage_all_at10"] = round(
        sum(r["coverage_all_at10"] for r in rows) / len(rows) * 100, 1
    )
    import statistics

    out["search_ms_mean"] = round(statistics.fmean(r["search_ms"] for r in rows), 1)
    if rows and "context_recall_any" in rows[0]:
        out["context_recall_any"] = round(
            sum(r["context_recall_any"] for r in rows) / len(rows) * 100, 1
        )
        out["context_coverage_all"] = round(
            sum(r["context_coverage_all"] for r in rows) / len(rows) * 100, 1
        )
        out["context_chars_mean"] = round(
            statistics.fmean(len(r["context"]) for r in rows), 1
        )
        out["context_sources_mean"] = round(
            statistics.fmean(
                r.get("context_diagnostics", {}).get("sourcesIncluded", 0)
                for r in rows
            ),
            1,
        )
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
