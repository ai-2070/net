## The helper in Python

The helper lives on the underlying `net` PyO3 module. `net_sdk`'s `NetNode`
wrapper does not re-export it, so import it directly:

```python
from net import RedisStreamDedup

dedup = RedisStreamDedup(capacity=600_000)

for entry_id, fields in r.xrange("net:shard:0", "0", "+"):
    if not dedup.is_duplicate(fields[b"dedup_id"].decode()):
        process(entry_id, fields)
```

`RedisStreamDedup(capacity=None)`, `is_duplicate(dedup_id) -> bool`, `len()`,
`capacity()`. The hot path releases the GIL.
