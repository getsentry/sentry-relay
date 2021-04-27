from datetime import datetime, timedelta, timezone
import json

from .test_metrics import TEST_CONFIG
from .test_envelope import generate_transaction_item


def test_session_metrics_feature_disabled(mini_sentry, relay):
    relay = relay(mini_sentry, options=TEST_CONFIG)

    project_id = 42
    mini_sentry.add_basic_project_config(project_id)

    timestamp = datetime.now(tz=timezone.utc)
    started = timestamp - timedelta(hours=1)
    session_payload = {
        "sid": "8333339f-5675-4f89-a9a0-1c935255ab58",
        "did": "foobarbaz",
        "seq": 42,
        "init": True,
        "timestamp": timestamp.isoformat(),
        "started": started.isoformat(),
        "duration": 1947.49,
        "status": "exited",
        "errors": 0,
        "attrs": {
            "release": "sentry-test@1.0.0",
            "environment": "production",
        },
    }

    relay.send_session(project_id, session_payload)

    # Get session envelope
    mini_sentry.captured_events.get(timeout=2)

    # Get metrics envelope
    assert mini_sentry.captured_events.empty()


def test_session_metrics(mini_sentry, relay):
    relay = relay(mini_sentry, options=TEST_CONFIG)

    project_id = 42
    mini_sentry.add_basic_project_config(project_id)

    mini_sentry.project_configs[project_id]["config"]["features"] = [
        "organizations:metrics-extraction"
    ]

    timestamp = datetime.now(tz=timezone.utc)
    started = timestamp - timedelta(hours=1)
    session_payload = {
        "sid": "8333339f-5675-4f89-a9a0-1c935255ab58",
        "did": "foobarbaz",
        "seq": 42,
        "init": True,
        "timestamp": timestamp.isoformat(),
        "started": started.isoformat(),
        "duration": 1947.49,
        "status": "exited",
        "errors": 0,
        "attrs": {
            "release": "sentry-test@1.0.0",
            "environment": "production",
        },
    }

    relay.send_session(project_id, session_payload)

    # Get session envelope
    mini_sentry.captured_events.get(timeout=2)

    # Get metrics envelope
    envelope = mini_sentry.captured_events.get(timeout=2)

    assert len(envelope.items) == 1

    metrics_item = envelope.items[0]
    assert metrics_item.type == "metric_buckets"

    received_metrics = metrics_item.get_bytes()
    assert json.loads(received_metrics.decode()) == [
        {
            "timestamp": int(timestamp.timestamp()),
            "name": "session",
            "type": "c",
            "value": 1.0,
            "tags": {
                "environment": "production",
                "release": "sentry-test@1.0.0",
                "session.status": "init",
            },
        },
        {
            "timestamp": int(timestamp.timestamp()),
            "name": "user",
            "type": "s",
            "value": [1617781333],
            "tags": {
                "environment": "production",
                "release": "sentry-test@1.0.0",
                "session.status": "init",
            },
        },
    ]


def test_transaction_metrics(mini_sentry, relay_with_processing, metrics_consumer):

    metrics_consumer = metrics_consumer()

    for feature_enabled in (True, False):

        relay = relay_with_processing(options=TEST_CONFIG)
        project_id = 42
        mini_sentry.add_full_project_config(project_id)
        timestamp = datetime.now(tz=timezone.utc)

        mini_sentry.project_configs[project_id]["config"]["features"] = (
            ["organizations:metrics-extraction"] if feature_enabled else []
        )

        transaction = generate_transaction_item()
        transaction["timestamp"] = timestamp.isoformat()
        transaction["measurements"] = {
            "foo": {"value": 1.2},
            "bar": {"value": 1.3},
        }
        transaction["breakdowns"] = {
            "breakdown1": {
                "baz": {"value": 1.4},
            }
        }

        relay.send_event(42, transaction)

        # Send another transaction:
        transaction["measurements"] = {
            "foo": {"value": 2.2},
        }
        transaction["breakdowns"] = {
            "breakdown1": {
                "baz": {"value": 2.4},
            }
        }
        relay.send_event(42, transaction)

        if not feature_enabled:
            message = metrics_consumer.poll(timeout=None)
            assert message is None, message.value()

            continue

        metrics = {
            metric["name"]: metric
            for metric in [metrics_consumer.get_metric() for _ in range(3)]
        }

        metrics_consumer.assert_empty()

        assert "measurement.foo" in metrics
        assert metrics["measurement.foo"] == {
            "org_id": 1,
            "project_id": 42,
            "timestamp": int(timestamp.timestamp()),
            "name": "measurement.foo",
            "type": "d",
            "unit": "",
            "value": [1.2, 2.2],
        }

        assert metrics["measurement.bar"] == {
            "org_id": 1,
            "project_id": 42,
            "timestamp": int(timestamp.timestamp()),
            "name": "measurement.bar",
            "type": "d",
            "unit": "",
            "value": [1.3],
        }

        assert metrics["breakdown.breakdown1.baz"] == {
            "org_id": 1,
            "project_id": 42,
            "timestamp": int(timestamp.timestamp()),
            "name": "breakdown.breakdown1.baz",
            "type": "d",
            "unit": "",
            "value": [1.4, 2.4],
        }
