## The helper in Go

```go
d := net.NewRedisStreamDedup(600_000)
defer d.Close()

for _, entry := range entries {
    if d.IsDuplicate(entry["dedup_id"]) {
        continue
    }
    process(entry)
}
```

`NewRedisStreamDedup(capacity uint)` — `0` selects the default. Then
`IsDuplicate(string) bool`, `IsDuplicateChecked(string) (bool, error)` (which
reports a closed handle or an embedded NUL rather than swallowing it), `Len()`,
`Capacity()`, `IsEmpty()`, `Clear()`, and `Close()`. A finalizer calls `Close`,
but close it explicitly.
