module github.com/ai-2070/net/go/example

// Must not trail the module this example replaces: `go/go.mod` declares
// `go 1.26`, and a lower directive here makes the checked-in example
// unrunnable as supplied ("module .. requires go >= 1.26").
go 1.26

require github.com/ai-2070/net/go v0.0.0

replace github.com/ai-2070/net/go => ..
