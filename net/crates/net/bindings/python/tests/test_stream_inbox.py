"""`NetMesh.open_stream_inbox`: every event on a stream, with the peer whose
session authenticated it — what `poll` cannot say (its events have no
sender).
"""

import pytest

pytest.importorskip("net._net")

from net import NetMesh, stream_id_from_label  # noqa: E402


def test_stream_id_from_label_is_stable_and_marks_a_stream() -> None:
    sid = stream_id_from_label("store/my-game.world")
    assert sid == stream_id_from_label("store/my-game.world")
    assert sid & (1 << 49)
    assert not sid & (1 << 48)
    # Any string is a label; nothing is validated.
    assert isinstance(stream_id_from_label("not a channel name!"), int)


def test_an_inbox_delivers_each_event_with_its_sender(mesh_pair) -> None:
    client, host = mesh_pair
    sid = stream_id_from_label("store/python-inbox-test")
    with host.open_stream_inbox(sid, capacity=16) as inbox:
        assert inbox.stream_id == sid
        with pytest.raises(RuntimeError, match="already has a receiver"):
            host.open_stream_inbox(sid)

        stream = client.open_stream(host.node_id, sid, reliability="reliable")
        client.send_with_retry(stream, [b"one", b"two"])

        first = inbox.recv(timeout_ms=5000)
        second = inbox.recv(timeout_ms=5000)
        assert first is not None and second is not None
        assert (first.peer_node_id, first.payload) == (client.node_id, b"one")
        assert (second.peer_node_id, second.payload) == (client.node_id, b"two")
        assert first.stream_id == sid
        assert inbox.try_recv() is None
        assert inbox.recv(timeout_ms=50) is None
        assert inbox.dropped == 0
    # Leaving the block closed it; the stream is free again.
    assert inbox.close() is False
    host.open_stream_inbox(sid).close()


def test_a_closed_inbox_answers_none_without_waiting(mesh_pair) -> None:
    _, host = mesh_pair
    inbox = host.open_stream_inbox(stream_id_from_label("store/python-inbox-closed"))
    assert inbox.close() is True
    assert inbox.recv() is None


def test_net_mesh_is_importable_alongside() -> None:
    assert NetMesh is not None
