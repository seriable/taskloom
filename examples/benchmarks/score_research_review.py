"""Aggregate explicit source-support annotations bound to saved native output.

Labels are supplied by the named reviewer; this script checks identity/ranges
and computes descriptive counts. It cannot independently verify semantic labels
or turn exact quote checks into entailment. Never use it as a blind benchmark.
"""
import argparse
import hashlib
import json
from pathlib import Path


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def score(campaign, review):
    if review['version'] != 1 or review['suite_sha256'] != campaign['suite_sha256'] or not review['reviewer']:
        raise ValueError('unsupported or mismatched review identity')
    observed = {c['id']: c for c in campaign['cases']}
    summary = {'evidence_kind': 'aggregation of supplied non-blind annotations, not a new native judge API call',
               'reviewer': review['reviewer'], 'reviewed_claims': 0, 'cited_support': {}, 'corpus_factuality': {},
               'cases': [], 'native_cases_without_final': [], 'unreviewed_final_reports': [],
               'limits': review['limitations']}
    seen = set()
    for case in review['cases']:
        if case['id'] in seen:
            raise ValueError('duplicate review case')
        seen.add(case['id'])
        native = observed[case['id']]
        final = native['final_output']
        if not isinstance(final, dict) or fingerprint(final) != case['response_sha256']:
            raise ValueError('reviewed native output changed or is missing')
        known_ids = {c['chunk_id'] for c in final['citations']}
        labels = {'supported', 'contradicted', 'insufficient'}
        for claim in case['claims']:
            if (not 0 <= claim['start'] < claim['end'] <= len(final['text'])
                    or final['text'][claim['start']:claim['end']] != claim['span']
                    or not set(claim['citation_ids']) <= known_ids
                    or claim['cited_support'] not in labels or claim['corpus_factuality'] not in labels
                    or not claim['reason']):
                raise ValueError('invalid claim range, citation identity or annotation label')
            summary['reviewed_claims'] += 1
            for key in ['cited_support', 'corpus_factuality']:
                label = claim[key]
                summary[key][label] = summary[key].get(label, 0) + 1
        summary['cases'].append({'id':case['id'], 'native_status':native['status'],
                                  'claims':len(case['claims']), 'omissions':case['omissions'],
                                  'reviewer_assessment':case.get('reviewer_assessment')})
    for identity, native in observed.items():
        if not isinstance(native['final_output'], dict):
            summary['native_cases_without_final'].append(identity)
        elif identity not in seen:
            summary['unreviewed_final_reports'].append(identity)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('campaign', type=Path, help='saved native report JSON')
    parser.add_argument('review', type=Path)
    args = parser.parse_args()
    campaign = json.loads(args.campaign.read_text(encoding='utf-8'))
    review = json.loads(args.review.read_text(encoding='utf-8'))
    print(json.dumps(score(campaign, review), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
