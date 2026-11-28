"""Keep latency sample denominators and documented nearest-rank tails honest."""
import sys
from pathlib import Path
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'examples/operations'))
from network_capacity_drill import distribution


class CapacityMeasurements(unittest.TestCase):
    def test_full_population_and_slow_tail_are_retained(self):
        measured = distribution([1000, *range(15)])
        self.assertEqual(measured['samples'], 16)
        self.assertEqual(measured['p50_ms'], 7)
        self.assertEqual(measured['p95_ms'], 1000)
        self.assertEqual(measured['max_ms'], 1000)

    def test_missing_population_cannot_look_like_a_zero_latency_success(self):
        with self.assertRaisesRegex(ValueError, 'denominator is empty'):
            distribution([])


if __name__ == '__main__':
    unittest.main()
