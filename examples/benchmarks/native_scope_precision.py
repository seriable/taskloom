"""Locate applicability exclusions for independent source-scope review.

This offline screen never rewrites an answer or labels it supported/false. A
match remains review-required even if evidence contains the same scope word.
No match is not an entailment pass: paraphrases and other hallucinations can
escape these deliberately bounded lexical patterns.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re


APPLICABILITY_EXCLUSION = re.compile(
    r'\b(?:apply|applies|applicable|valid|available|usable|works?)\s+'
    r'(?:only|solely|exclusively)\b|'
    r'\b(?:only|solely|exclusively)\s+(?:apply|applies|works?)\b|'
    r'\b(?:does|do|will|can)\s+not\s+(?:apply|work|be used)\b|'
    r'\b(?:never|cannot)\s+(?:apply|applies|be used|work|works)\b|'
    r'\b(?:exclusive|limited|restricted)\s+to\b|\bnot\s+applicable\b',
    re.IGNORECASE,
)
SOURCE_SCOPE_WORD = re.compile(r'\b(?:only|never|exclusive|solely)\b', re.IGNORECASE)


def screen_scope(text, evidence):
    """Return exact original offsets and source leads, never a truth verdict."""
    source_leads = []
    for source in evidence:
        for match in SOURCE_SCOPE_WORD.finditer(source['text']):
            start = source['text'].rfind('\n', 0, match.start()) + 1
            end = source['text'].find('\n', match.end())
            end = len(source['text']) if end < 0 else end
            item = {'chunk_id': source['id'], 'start': start, 'end': end,
                    'text': source['text'][start:end]}
            if item not in source_leads:
                source_leads.append(item)
    findings = []
    for match in APPLICABILITY_EXCLUSION.finditer(text):
        start = text.rfind('\n', 0, match.start()) + 1
        end = text.find('\n', match.end())
        end = len(text) if end < 0 else end
        findings.append({'match_start': match.start(), 'match_end': match.end(),
                         'matched_text': match.group(), 'paragraph_start': start,
                         'paragraph_end': end, 'paragraph': text[start:end],
                         'disposition': 'independent source-scope review required'})
    return {'kind': 'bounded lexical applicability-scope screen, not entailment',
            'original_text_sha256': hashlib.sha256(text.encode()).hexdigest(),
            'review_required': bool(findings), 'findings': findings,
            'possible_source_leads': source_leads,
            'source_lead_qualification': 'Scope words in a source are leads only; they do not establish that an answer exclusion is entailed.',
            'answer_modified': False,
            'semantic_verdict': None,
            'limits': 'Absence of a match does not establish support or complete task coverage. Source-grounded exclusions may also be flagged; independent review must read the actual statement and its conditions.'}


def audit_campaign(path):
    """Read immutable native DAG final children and their own executed evidence."""
    raw = path.read_bytes()
    if len(raw) > 32 * 1024 * 1024:
        raise ValueError('campaign exceeds offline 32 MiB bound')
    campaign = json.loads(raw)
    cases = []
    for case in campaign['cases']:
        if case['request'].get('peer') is not None:
            raise ValueError('this saved-report reader requires an explicit DAG final draft')
        draft = next(n for n in case['observation']['state']['nodes'] if n['id'] == 'draft')
        child = next(c for c in case['children'] if c['report']['run_id'] == draft['child']['run_id'])
        message = child['report']['messages'][-1]
        if message['role'] != 'assistant' or message['turn']['tool_calls']:
            raise ValueError('final child has no terminal original assistant text')
        original = json.loads(message['turn']['content'])
        selected = original['sources']
        if not selected or len(set(selected)) != len(selected):
            raise ValueError('nonempty unique original selected-source identities required')
        calls = {step['call']['id'] for step in child['tool_steps']
                 if step['call']['name'] == 'knowledge_search' and step['progress']['type'] == 'complete'}
        chunks = {}
        for observed in child['report']['messages']:
            if (observed['role'] != 'tool' or observed['result']['call_id'] not in calls
                    or observed['result']['is_error']):
                continue
            for hit in observed['result']['output']['hits']:
                chunk = hit['chunk']
                if chunk['id'] in selected:
                    if chunk['id'] in chunks and chunks[chunk['id']] != chunk:
                        raise ValueError('conflicting source identity in saved evidence')
                    chunks[chunk['id']] = chunk
        if set(chunks) != set(selected):
            raise ValueError('selected source missing from this child executed retrieval')
        result = screen_scope(original['text'], [chunks[identity] for identity in selected])
        result.update(case_id=case['id'], writer_run_id=child['report']['run_id'],
                      source_chunk_ids=selected,
                      original_model_json_sha256=hashlib.sha256(message['turn']['content'].encode()).hexdigest())
        cases.append(result)
    return {'version': 1, 'campaign_report': str(path),
            'campaign_report_sha256': hashlib.sha256(raw).hexdigest(),
            'cases': cases, 'new_model_requests': 0,
            'original_report_modified': False,
            'acceptance_qualification': 'An additional offline review aid, not a replacement for native provenance, existing task gates or independent semantic assessment.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--campaign-report', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    if args.out.resolve() == args.campaign_report.resolve():
        raise ValueError('screen output must not replace the original report')
    result = audit_campaign(args.campaign_report)
    with args.out.open('x', encoding='utf-8') as output:
        json.dump(result, output, ensure_ascii=False, indent=2)
        output.write('\n')
    print(json.dumps({'cases': len(result['cases']),
                      'review_required': sum(c['review_required'] for c in result['cases']),
                      'semantic_verdict': None, 'new_model_requests': 0}))


if __name__ == '__main__':
    main()
