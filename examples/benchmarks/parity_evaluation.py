"""Offline parity preparation and separate quality scoring.

No provider calls or generated labels. A manifest equality check establishes
declared conditions only; it cannot attest that an adapter executed them.
Human annotation identity/independence is a self-declaration requiring external
review, never something this script or a coding Agent can manufacture.
"""
import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path


DIMENSIONS = {
    'exact_provenance': {'verified', 'failed', 'missing', 'error'},
    'source_support': {'supported', 'contradicted', 'insufficient', 'error'},
    'corpus_factuality': {'supported', 'contradicted', 'insufficient', 'error'},
    'task_coverage': {'complete', 'partial', 'missing', 'error'},
}
SUCCESS = {'exact_provenance': 'verified', 'source_support': 'supported',
           'corpus_factuality': 'supported', 'task_coverage': 'complete'}
CONDITION_KEYS = {'model', 'tasks', 'corpus_sha256', 'system_prompts_sha256',
                  'tool_schema_sha256', 'tool_grants_sha256', 'output_schema_sha256',
                  'output_tokens', 'max_http_attempts_per_case', 'per_case_budget',
                  'retry_policy', 'deadline_seconds', 'retrieval_policy_sha256'}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False,
                                     separators=(',', ':')).encode()).hexdigest()


def read(path):
    return json.loads(path.read_text(encoding='utf-8-sig'))


def create(path, value):
    with path.open('x', encoding='utf-8') as output:
        json.dump(value, output, ensure_ascii=False, indent=2)
        output.write('\n')


def validate_suite(suite, root):
    if suite.get('version') != 1 or not suite.get('queries') or not suite.get('documents'):
        raise ValueError('nonempty version-1 suite required')
    documents = {d['id']: d for d in suite['documents']}
    if len(documents) != len(suite['documents']):
        raise ValueError('duplicate document ID')
    queries = {q['id']: q for q in suite['queries']}
    if len(queries) != len(suite['queries']):
        raise ValueError('duplicate query ID')
    required_groups = {'chinese', 'english', 'mixed', 'short', 'paraphrase',
                       'negative', 'identifier', 'cross_domain'}
    seen = set()
    for query in queries.values():
        if not query['text'].strip() or not set(query['relevant']) <= set(documents):
            raise ValueError('empty query or unknown relevance document')
        if len(set(query['relevant'])) != len(query['relevant']):
            raise ValueError('duplicate relevance document')
        if not query.get('groups') or not query.get('label_author'):
            raise ValueError('query needs groups and explicit label author')
        seen.update(query['groups'])
    if not required_groups <= seen:
        raise ValueError('missing required multilingual/domain query groups')
    for pin in suite.get('source_pins', []):
        path = (root / pin['path']).resolve()
        if not path.is_relative_to(root.resolve()):
            raise ValueError('source path escapes repository')
        text = path.read_text(encoding='utf-8')
        if (hashlib.sha256(text.encode()).hexdigest() != pin['sha256_utf8_lf']
                or documents[pin['document_id']]['text'] != text):
            raise ValueError('source drift before any dispatch: '+pin['path'])
    return suite


def retrieval_dataset(suite, root):
    validate_suite(suite, root)
    # Compatible with native_retrieval.py. This file is host evaluation input:
    # relevant labels stay in the evaluator, never a model request.
    return {'version': 2, 'provenance': suite['provenance'],
            'suite_sha256_canonical': digest(suite),
            'documents': suite['documents'], 'source_pins': suite.get('source_pins', []),
            'queries': suite['queries']}


def model_inputs(suite):
    """Explicit allowlist: provider-facing queries contain no labels or gold."""
    return [{'id': q['id'], 'query': q['text']} for q in suite['queries']]


def validate_manifest(manifest):
    if manifest.get('version') != 1 or not isinstance(manifest.get('framework'), dict):
        raise ValueError('version/framework required')
    framework = manifest['framework']
    if not all(isinstance(framework.get(k), str) and framework[k].strip()
               for k in ['name', 'version', 'source_identity']):
        raise ValueError('actual framework identity required')
    conditions = manifest.get('conditions', {})
    if set(conditions) != CONDITION_KEYS:
        raise ValueError('all exact condition fields required, no implicit defaults')
    model = conditions['model']
    if (not isinstance(model, dict) or set(model) != {'provider', 'endpoint_identity', 'model', 'parameters'}
            or not all(isinstance(model[k], str) and model[k] for k in ['provider', 'endpoint_identity', 'model'])
            or not isinstance(model['parameters'], dict)):
        raise ValueError('explicit provider/model/endpoint/parameters required')
    if (not isinstance(conditions['tasks'], list) or not conditions['tasks']
            or len(set(conditions['tasks'])) != len(conditions['tasks'])):
        raise ValueError('ordered unique task identities required')
    for key in ['corpus_sha256', 'system_prompts_sha256', 'tool_schema_sha256',
                'tool_grants_sha256', 'output_schema_sha256', 'retrieval_policy_sha256']:
        value = conditions[key]
        if not isinstance(value, str) or len(value) != 64 or any(c not in '0123456789abcdef' for c in value):
            raise ValueError('exact SHA-256 required: '+key)
    for key in ['output_tokens', 'max_http_attempts_per_case', 'deadline_seconds']:
        if type(conditions[key]) is not int or conditions[key] <= 0:
            raise ValueError('positive finite bound required: '+key)
    budget = conditions['per_case_budget']
    if (not isinstance(budget, dict) or set(budget) != {'currency', 'max_cost_micros', 'prices_sha256'}
            or not isinstance(budget['currency'], str) or not budget['currency']
            or type(budget['max_cost_micros']) is not int or budget['max_cost_micros'] < 0
            or not isinstance(budget['prices_sha256'], str) or len(budget['prices_sha256']) != 64):
        raise ValueError('explicit finite cost/prices binding required')
    if not isinstance(conditions['retry_policy'], dict) or not conditions['retry_policy']:
        raise ValueError('explicit retry/repair policy required')
    return manifest


def compare_manifests(reference, candidate):
    validate_manifest(reference)
    validate_manifest(candidate)
    different = [key for key in sorted(CONDITION_KEYS)
                 if digest(reference['conditions'][key]) != digest(candidate['conditions'][key])]
    return {'status': 'not_comparable' if different else 'declared_conditions_equal',
            'differences': different, 'performance_rank_allowed': False,
            'reference_framework': reference['framework'], 'candidate_framework': candidate['framework'],
            'qualification': 'Exact declared conditions are necessary but not execution evidence. Actual native calls, adapter provenance, unknown usage and complete cases are still required. No framework was run by this offline comparison.'}


def validate_annotations(document, expected_cases):
    if document.get('version') != 1 or document.get('kind') != 'external_human_annotations':
        raise ValueError('external human annotation format required')
    author = document.get('author', {})
    if (set(author) != {'id', 'kind', 'independent', 'affiliation', 'declaration'}
            or author['kind'] != 'human' or author['independent'] is not True
            or not all(isinstance(author[k], str) and author[k].strip()
                       for k in ['id', 'affiliation', 'declaration'])):
        raise ValueError('named human independence declaration required; Agents cannot supply it')
    seen = set()
    for item in document.get('cases', []):
        if item['id'] not in expected_cases or item['id'] in seen:
            raise ValueError('unknown/duplicate annotated case')
        seen.add(item['id'])
        if item.get('author') != author['id']:
            raise ValueError('every case must bind its author')
        conflict = item.get('conflict', {})
        if conflict.get('status') not in {'none', 'open', 'resolved'} or not isinstance(conflict.get('note'), str):
            raise ValueError('explicit conflict status/note required')
        if not isinstance(item.get('coverage'), dict) or set(item['coverage']) != set(DIMENSIONS):
            raise ValueError('all separate dimensions must be covered or explicitly missing')
        if (not isinstance(item.get('output_sha256'), str) or len(item['output_sha256']) != 64
                or any(c not in '0123456789abcdef' for c in item['output_sha256'])):
            raise ValueError('exact original answer identity required')
        for dimension, labels in DIMENSIONS.items():
            label = item['coverage'][dimension]
            if label is not None and label not in labels:
                raise ValueError('invalid dimension label')
        if not isinstance(item.get('reason'), str) or not item['reason'].strip():
            raise ValueError('each human annotation needs source-relative rationale')
    return {'annotated': len(seen), 'missing': sorted(set(expected_cases)-seen),
            'human_identity_verified': False, 'independence_verified': False,
            'qualification': 'Format checked only. Human identity and independence are external self-declarations, not attested by this script or a coding Agent.'}


def annotation_agreement(expected_cases, documents):
    """Pairwise nominal Cohen kappa, with missing coverage reported separately.

    Agreement is not correctness. Missing/error/open-conflict labels never become
    matching categories, and a different output hash cannot become disagreement.
    """
    if (not expected_cases or len(set(expected_cases)) != len(expected_cases)
            or len(expected_cases) > 10000 or not 2 <= len(documents) <= 16):
        raise ValueError('unique frozen cases and2..16 distinct annotation authors required')
    validations = [validate_annotations(document, set(expected_cases)) for document in documents]
    authors = [document['author']['id'] for document in documents]
    if len(set(authors)) != len(authors):
        raise ValueError('one document per distinct author; duplicate raters would inflate agreement')
    rows = [{row['id']: row for row in document['cases']} for document in documents]
    for identity in expected_cases:
        hashes = {row[identity]['output_sha256'] for row in rows if identity in row}
        if len(hashes) > 1:
            raise ValueError('annotators rated different original outputs: '+identity)
    pairs = []
    for i, left in enumerate(rows):
        for j in range(i+1, len(rows)):
            right = rows[j]
            dimensions = {}
            for dimension in DIMENSIONS:
                labels, excluded = [], Counter()
                for identity in expected_cases:
                    a, b = left.get(identity), right.get(identity)
                    if a is None or b is None:
                        excluded['missing_annotation'] += 1
                    elif a['conflict']['status'] == 'open' or b['conflict']['status'] == 'open':
                        excluded['open_conflict'] += 1
                    elif a['coverage'][dimension] is None or b['coverage'][dimension] is None:
                        excluded['missing_label'] += 1
                    elif 'error' in (a['coverage'][dimension], b['coverage'][dimension]):
                        excluded['annotation_error'] += 1
                    else:
                        labels.append((a['coverage'][dimension], b['coverage'][dimension]))
                count = len(labels)
                observed = sum(a == b for a, b in labels)/count if count else None
                lm, rm = Counter(a for a, _ in labels), Counter(b for _, b in labels)
                chance = sum(lm[key]*rm[key] for key in set(lm)|set(rm))/(count*count) if count else None
                kappa = (observed-chance)/(1-chance) if count and chance < 1 else None
                dimensions[dimension] = {'expected_cases': len(expected_cases), 'paired_valid': count,
                    'excluded': dict(excluded), 'coverage': count/len(expected_cases),
                    'observed_agreement': observed, 'chance_agreement': chance, 'cohen_kappa': kappa,
                    'undefined_reason': 'no_paired_labels' if not count else ('constant_label_marginals' if chance == 1 else None)}
            pairs.append({'authors': [authors[i], authors[j]], 'dimensions': dimensions})
    return {'version': 1, 'expected_cases': len(expected_cases), 'pairs': pairs,
            'author_format_validation': validations, 'human_identity_verified': False,
            'independence_verified': False, 'is_correctness_score': False,
            'qualification': 'Pairwise nominal agreement on the same frozen answers only. Missing/conflicted/error annotations reduce coverage; no consensus labels, objective truth, identity attestation or quality ranking is inferred.'}


def score_answers(expected_cases, outputs, annotations):
    if not expected_cases or len(set(expected_cases)) != len(expected_cases):
        raise ValueError('nonempty unique expected cases required')
    validation = validate_annotations(annotations, set(expected_cases))
    rows = {}
    for output in outputs:
        if output['id'] not in expected_cases or output['id'] in rows:
            raise ValueError('unknown/duplicate original output')
        rows[output['id']] = output
    labels = {case['id']: case for case in annotations['cases']}
    details = []
    counts = {dimension: 0 for dimension in DIMENSIONS}
    for identity in expected_cases:
        output, annotated = rows.get(identity), labels.get(identity)
        status = 'scored'
        if output is None or 'error' in output:
            status = 'missing_output' if output is None else 'execution_error'
        elif annotated is None:
            status = 'missing_annotation'
        elif hashlib.sha256(output['text'].encode()).hexdigest() != annotated['output_sha256']:
            raise ValueError('annotation refers to altered output: '+identity)
        elif annotated['conflict']['status'] == 'open':
            status = 'unresolved_annotation_conflict'
        verdicts = {}
        for dimension in DIMENSIONS:
            label = annotated['coverage'][dimension] if status == 'scored' else None
            passed = label == SUCCESS[dimension]
            counts[dimension] += passed
            verdicts[dimension] = {'label': label, 'passed': passed}
        details.append({'id': identity, 'status': status, 'dimensions': verdicts})
    return {'expected_cases': len(expected_cases), 'dimensions': {
        dimension: {'passed': count, 'denominator': len(expected_cases), 'rate': count/len(expected_cases)}
        for dimension, count in counts.items()}, 'cases': details,
        'annotations': validation, 'factuality_scope': 'Relative to supplied corpus, not objective real-world truth.',
        'missing_or_error_excluded': False}


def score_retrieval(suite, observed):
    expected = {query['id']: query for query in suite['queries']}
    actual = {}
    for case in observed:
        if case['id'] not in expected or case['id'] in actual:
            raise ValueError('unknown/duplicate retrieval case')
        actual[case['id']] = case
    rows = []
    for identity, query in expected.items():
        case = actual.get(identity)
        ranked = [] if case is None or 'error' in case else case.get('ranked', [])
        if not isinstance(ranked, list) or any(not isinstance(x, str) for x in ranked):
            raise ValueError('ranking must be document IDs')
        ranked = list(dict.fromkeys(ranked))[:5]
        relevant = set(query['relevant'])
        error = case is None or 'error' in case
        rows.append({'id': identity, 'groups': query['groups'], 'positive': bool(relevant),
                     'recall': len(set(ranked)&relevant)/len(relevant) if relevant else None,
                     'rr': next((1/(i+1) for i, value in enumerate(ranked) if value in relevant), 0),
                     'negative_empty': not error and not ranked and not relevant,
                     'error_or_missing': error})
    def aggregate(cases):
        positives = [row for row in cases if row['positive']]
        negatives = [row for row in cases if not row['positive']]
        return {'cases': len(cases), 'positive_queries': len(positives), 'negative_queries': len(negatives),
                'recall_at_5': sum(row['recall'] for row in positives)/len(positives) if positives else None,
                'mrr_at_5': sum(row['rr'] for row in positives)/len(positives) if positives else None,
                'negative_empty': sum(row['negative_empty'] for row in negatives),
                'errors_or_missing': sum(row['error_or_missing'] for row in cases)}
    return {'all': aggregate(rows), 'groups': {group: aggregate([row for row in rows if group in row['groups']])
            for group in sorted({group for row in rows for group in row['groups']})},
            'cases': rows, 'quality_qualification': 'Maintainer-labelled observed development queries, not independently held-out or human-verified; no semantic entailment score.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    dataset = commands.add_parser('dataset')
    dataset.add_argument('--suite', type=Path, required=True)
    dataset.add_argument('--out', type=Path, required=True)
    inputs = commands.add_parser('model-inputs')
    inputs.add_argument('--suite', type=Path, required=True)
    inputs.add_argument('--out', type=Path, required=True)
    parity = commands.add_parser('compare')
    parity.add_argument('--reference', type=Path, required=True)
    parity.add_argument('--candidate', type=Path, required=True)
    parity.add_argument('--out', type=Path, required=True)
    answers = commands.add_parser('score-answers')
    answers.add_argument('--expected', type=Path, required=True, help='JSON ordered list of case IDs')
    answers.add_argument('--outputs', type=Path, required=True, help='JSON list of original {id,text} or {id,error}')
    answers.add_argument('--annotations', type=Path, required=True)
    answers.add_argument('--out', type=Path, required=True)
    agreement = commands.add_parser('agreement')
    agreement.add_argument('--expected', type=Path, required=True)
    agreement.add_argument('--annotations', type=Path, action='append', required=True)
    agreement.add_argument('--out', type=Path, required=True)
    retrieval = commands.add_parser('score-retrieval')
    retrieval.add_argument('--suite', type=Path, required=True)
    retrieval.add_argument('--report', type=Path, required=True, help='native_retrieval report')
    retrieval.add_argument('--phase', choices=['lexical', 'hybrid', 'default_lexical'], default='hybrid')
    retrieval.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    if args.command in {'dataset', 'model-inputs', 'score-retrieval'}:
        suite = validate_suite(read(args.suite), root)
    if args.command == 'dataset': result = retrieval_dataset(suite, root)
    elif args.command == 'model-inputs': result = model_inputs(suite)
    elif args.command == 'compare': result = compare_manifests(read(args.reference), read(args.candidate))
    elif args.command == 'score-answers': result = score_answers(read(args.expected), read(args.outputs), read(args.annotations))
    elif args.command == 'agreement': result = annotation_agreement(read(args.expected), [read(path) for path in args.annotations])
    else: result = score_retrieval(suite, read(args.report)[args.phase]['cases'])
    create(args.out, result)
    print(json.dumps({'command': args.command, 'output': str(args.out), 'provider_requests': 0}))


if __name__ == '__main__':
    main()
