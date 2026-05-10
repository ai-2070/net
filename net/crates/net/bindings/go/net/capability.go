// Capability-System Enhancements — Go reference implementation.
//
// Mirrors the substrate's typed-tag taxonomy, predicate IR, and
// CapabilitySet diff exactly, so Go applications produce byte-equal
// wire JSON to the Rust + TS + Python SDKs. The fixtures under
// net/crates/net/tests/cross_lang_capability/ pin the canonical
// shapes and capability_test.go drives them.
//
// The wire format is { "tags": [...], "metadata": {...} } — pure
// JSON, no FFI dance required for the predicate IR / diff / tag
// taxonomy. The substrate's capability-index lookup + predicate
// evaluation against the live index stays Rust-side; this surface
// produces the request shapes.

package net

import (
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strings"
	"sync/atomic"
)

// ============================================================================
// Typed taxonomy
// ============================================================================

// TaxonomyAxis is the canonical capability axis. Mirrors
// `TaxonomyAxis` in the substrate. The wire form is the lowercase
// string.
type TaxonomyAxis string

const (
	AxisHardware  TaxonomyAxis = "hardware"
	AxisSoftware  TaxonomyAxis = "software"
	AxisDevices   TaxonomyAxis = "devices"
	AxisDataforts TaxonomyAxis = "dataforts"
)

// TaxonomyAxes lists every axis the substrate knows about.
var TaxonomyAxes = []TaxonomyAxis{AxisHardware, AxisSoftware, AxisDevices, AxisDataforts}

// ReservedPrefixes — substrate-privileged-path cross-axis prefixes.
// User code goes through TagFromUserString which rejects these.
var ReservedPrefixes = []string{"causal:", "fork-of:", "heat:", "scope:"}

// AxisSeparator is the character between an axis-tag's key and value.
type AxisSeparator byte

const (
	SepEq    AxisSeparator = '='
	SepColon AxisSeparator = ':'
)

// TagKey is the {axis, key} addressing pair for axis-prefixed tags
// and axis-keyed predicates.
type TagKey struct {
	Axis TaxonomyAxis `json:"axis"`
	Key  string       `json:"key"`
}

// NewTagKey constructs a TagKey. Returns an error on empty key.
func NewTagKey(axis TaxonomyAxis, key string) (TagKey, error) {
	if key == "" {
		return TagKey{}, fmt.Errorf("NewTagKey: key must be non-empty (axis=%q)", axis)
	}
	return TagKey{Axis: axis, Key: key}, nil
}

// MustTagKey is the panicking variant — use only in test code or for
// compile-time-known constants.
func MustTagKey(axis TaxonomyAxis, key string) TagKey {
	tk, err := NewTagKey(axis, key)
	if err != nil {
		panic(err)
	}
	return tk
}

// TagKind discriminates the Tag struct.
type TagKind uint8

const (
	TagKindAxisPresent TagKind = iota
	TagKindAxisValue
	TagKindReserved
	TagKindLegacy
)

// Tag is the typed capability tag. Mirrors the substrate's `Tag`
// enum. Use NewAxisPresentTag / NewAxisValueTag / NewReservedTag /
// NewLegacyTag for construction; access fields via Kind().
type Tag struct {
	Kind   TagKind
	Axis   TaxonomyAxis
	Key    string
	Value  string
	Sep    AxisSeparator
	Prefix string
	Body   string
	Raw    string
}

// NewAxisPresentTag builds an axis-present tag (`<axis>.<key>`).
func NewAxisPresentTag(axis TaxonomyAxis, key string) Tag {
	return Tag{Kind: TagKindAxisPresent, Axis: axis, Key: key}
}

// NewAxisValueTag builds an axis-value tag (`<axis>.<key><sep><value>`).
func NewAxisValueTag(axis TaxonomyAxis, key, value string, sep AxisSeparator) Tag {
	return Tag{
		Kind:  TagKindAxisValue,
		Axis:  axis,
		Key:   key,
		Value: value,
		Sep:   sep,
	}
}

// NewReservedTag builds a reserved-prefix tag.
func NewReservedTag(prefix, body string) Tag {
	return Tag{Kind: TagKindReserved, Prefix: prefix, Body: body}
}

// NewLegacyTag builds a free-form legacy tag.
func NewLegacyTag(raw string) Tag {
	return Tag{Kind: TagKindLegacy, Raw: raw}
}

// String renders to canonical wire form. Matches the substrate's
// `Display` impl byte-for-byte.
func (t Tag) String() string {
	switch t.Kind {
	case TagKindAxisPresent:
		return string(t.Axis) + "." + t.Key
	case TagKindAxisValue:
		return string(t.Axis) + "." + t.Key + string(t.Sep) + t.Value
	case TagKindReserved:
		return t.Prefix + t.Body
	case TagKindLegacy:
		return t.Raw
	default:
		return ""
	}
}

// StartsWithReservedPrefix returns the matched prefix or empty
// string if none matches.
func StartsWithReservedPrefix(s string) string {
	for _, p := range ReservedPrefixes {
		if strings.HasPrefix(s, p) {
			return p
		}
	}
	return ""
}

func axisFromPrefix(s string) (TaxonomyAxis, bool) {
	for _, a := range TaxonomyAxes {
		if string(a) == s {
			return a, true
		}
	}
	return "", false
}

// TagFromString parses a wire string into a Tag. Privileged path —
// accepts reserved prefixes. User code should use TagFromUserString.
func TagFromString(s string) (Tag, error) {
	if s == "" {
		return Tag{}, errors.New("TagFromString: tag must be non-empty")
	}
	if reserved := StartsWithReservedPrefix(s); reserved != "" {
		return NewReservedTag(reserved, s[len(reserved):]), nil
	}
	dot := strings.IndexByte(s, '.')
	if dot < 0 {
		return NewLegacyTag(s), nil
	}
	axis, ok := axisFromPrefix(s[:dot])
	if !ok {
		return NewLegacyTag(s), nil
	}
	body := s[dot+1:]
	if body == "" {
		return NewLegacyTag(s), nil
	}
	eq := strings.IndexByte(body, '=')
	colon := strings.IndexByte(body, ':')
	sep := AxisSeparator(0)
	sepIdx := -1
	switch {
	case eq >= 0 && colon >= 0:
		if eq < colon {
			sep, sepIdx = SepEq, eq
		} else {
			sep, sepIdx = SepColon, colon
		}
	case eq >= 0:
		sep, sepIdx = SepEq, eq
	case colon >= 0:
		sep, sepIdx = SepColon, colon
	}
	if sep == 0 {
		return NewAxisPresentTag(axis, body), nil
	}
	key := body[:sepIdx]
	value := body[sepIdx+1:]
	if key == "" || value == "" {
		return NewLegacyTag(s), nil
	}
	return NewAxisValueTag(axis, key, value, sep), nil
}

// TagFromUserString rejects reserved prefixes, mirroring
// `Tag::parse_user`.
func TagFromUserString(s string) (Tag, error) {
	if s == "" {
		return Tag{}, errors.New("TagFromUserString: tag must be non-empty")
	}
	if reserved := StartsWithReservedPrefix(s); reserved != "" {
		return Tag{}, fmt.Errorf(
			"tag %q starts with reserved prefix %q; user code cannot emit reserved-prefix tags",
			s, reserved,
		)
	}
	return TagFromString(s)
}

// ============================================================================
// Predicate IR — flat post-order tree, identical wire shape to the
// substrate's PredicateWire and the cross-binding fixtures.
// ============================================================================

// PredicateNode is the wire representation of a single AST node. The
// JSON `kind` field discriminates; child indices reference earlier
// entries in the PredicateWire.Nodes slice.
type PredicateNode struct {
	Kind      string   `json:"kind"`
	Key       any      `json:"key,omitempty"` // TagKey for axis preds, string for metadata preds.
	Value     string   `json:"value,omitempty"`
	Threshold *float64 `json:"threshold,omitempty"`
	Min       *float64 `json:"min,omitempty"`
	Max       *float64 `json:"max,omitempty"`
	Version   string   `json:"version,omitempty"`
	Prefix    string   `json:"prefix,omitempty"`
	Pattern   string   `json:"pattern,omitempty"`
	Children  []int    `json:"children,omitempty"`
	Child     *int     `json:"child,omitempty"`
}

// PredicateWire is the canonical JSON shape — pinned by the
// `predicate_nrpc_envelope.json` cross-binding fixture.
type PredicateWire struct {
	Nodes   []PredicateNode `json:"nodes"`
	RootIdx int             `json:"root_idx"`
}

// Predicate is the in-memory AST. Sugar over PredicateWire — the
// `Pred` namespace constructs these and PredicateToWire flattens
// them.
type Predicate struct {
	kind      predKind
	key       TagKey
	mdKey     string
	value     string
	threshold float64
	min       float64
	max       float64
	version   string
	prefix    string
	pattern   string
	children  []*Predicate
	child     *Predicate
}

type predKind int

const (
	pkExists predKind = iota
	pkEquals
	pkNumericAtLeast
	pkNumericAtMost
	pkNumericInRange
	pkSemverAtLeast
	pkSemverAtMost
	pkSemverCompatible
	pkStringPrefix
	pkStringMatches
	pkMetadataExists
	pkMetadataEquals
	pkMetadataMatches
	pkMetadataNumericAtLeast
	pkAnd
	pkOr
	pkNot
)

// Pred is the fluent predicate-builder namespace. Usage:
//
//	pred := net.Pred.And(
//	    net.Pred.Exists(net.MustTagKey(net.AxisHardware, "gpu")),
//	    net.Pred.NumericAtLeast(net.MustTagKey(net.AxisHardware, "memory_mb"), 65536),
//	    net.Pred.MetadataEquals("intent", "ml-training"),
//	)
var Pred = predBuilder{}

type predBuilder struct{}

func (predBuilder) Exists(k TagKey) *Predicate {
	return &Predicate{kind: pkExists, key: k}
}
func (predBuilder) Equals(k TagKey, v string) *Predicate {
	return &Predicate{kind: pkEquals, key: k, value: v}
}
func (predBuilder) NumericAtLeast(k TagKey, t float64) *Predicate {
	return &Predicate{kind: pkNumericAtLeast, key: k, threshold: t}
}
func (predBuilder) NumericAtMost(k TagKey, t float64) *Predicate {
	return &Predicate{kind: pkNumericAtMost, key: k, threshold: t}
}
func (predBuilder) NumericInRange(k TagKey, mn, mx float64) *Predicate {
	return &Predicate{kind: pkNumericInRange, key: k, min: mn, max: mx}
}
func (predBuilder) SemverAtLeast(k TagKey, v string) *Predicate {
	return &Predicate{kind: pkSemverAtLeast, key: k, version: v}
}
func (predBuilder) SemverAtMost(k TagKey, v string) *Predicate {
	return &Predicate{kind: pkSemverAtMost, key: k, version: v}
}
func (predBuilder) SemverCompatible(k TagKey, v string) *Predicate {
	return &Predicate{kind: pkSemverCompatible, key: k, version: v}
}
func (predBuilder) StringPrefix(k TagKey, p string) *Predicate {
	return &Predicate{kind: pkStringPrefix, key: k, prefix: p}
}
func (predBuilder) StringMatches(k TagKey, p string) *Predicate {
	return &Predicate{kind: pkStringMatches, key: k, pattern: p}
}
func (predBuilder) MetadataExists(k string) *Predicate {
	return &Predicate{kind: pkMetadataExists, mdKey: k}
}
func (predBuilder) MetadataEquals(k, v string) *Predicate {
	return &Predicate{kind: pkMetadataEquals, mdKey: k, value: v}
}
func (predBuilder) MetadataMatches(k, p string) *Predicate {
	return &Predicate{kind: pkMetadataMatches, mdKey: k, pattern: p}
}
func (predBuilder) MetadataNumericAtLeast(k string, t float64) *Predicate {
	return &Predicate{kind: pkMetadataNumericAtLeast, mdKey: k, threshold: t}
}
func (predBuilder) And(children ...*Predicate) *Predicate {
	return &Predicate{kind: pkAnd, children: children}
}
func (predBuilder) Or(children ...*Predicate) *Predicate {
	return &Predicate{kind: pkOr, children: children}
}
func (predBuilder) Not(child *Predicate) *Predicate {
	return &Predicate{kind: pkNot, child: child}
}

func ptrFloat(v float64) *float64 { return &v }
func ptrInt(v int) *int           { return &v }

func emit(p *Predicate, out *[]PredicateNode) int {
	switch p.kind {
	case pkExists:
		*out = append(*out, PredicateNode{Kind: "exists", Key: p.key})
	case pkEquals:
		*out = append(*out, PredicateNode{Kind: "equals", Key: p.key, Value: p.value})
	case pkNumericAtLeast:
		*out = append(*out, PredicateNode{
			Kind:      "numeric_at_least",
			Key:       p.key,
			Threshold: ptrFloat(p.threshold),
		})
	case pkNumericAtMost:
		*out = append(*out, PredicateNode{
			Kind:      "numeric_at_most",
			Key:       p.key,
			Threshold: ptrFloat(p.threshold),
		})
	case pkNumericInRange:
		*out = append(*out, PredicateNode{
			Kind: "numeric_in_range",
			Key:  p.key,
			Min:  ptrFloat(p.min),
			Max:  ptrFloat(p.max),
		})
	case pkSemverAtLeast:
		*out = append(*out, PredicateNode{Kind: "semver_at_least", Key: p.key, Version: p.version})
	case pkSemverAtMost:
		*out = append(*out, PredicateNode{Kind: "semver_at_most", Key: p.key, Version: p.version})
	case pkSemverCompatible:
		*out = append(*out, PredicateNode{Kind: "semver_compatible", Key: p.key, Version: p.version})
	case pkStringPrefix:
		*out = append(*out, PredicateNode{Kind: "string_prefix", Key: p.key, Prefix: p.prefix})
	case pkStringMatches:
		*out = append(*out, PredicateNode{Kind: "string_matches", Key: p.key, Pattern: p.pattern})
	case pkMetadataExists:
		*out = append(*out, PredicateNode{Kind: "metadata_exists", Key: p.mdKey})
	case pkMetadataEquals:
		*out = append(*out, PredicateNode{Kind: "metadata_equals", Key: p.mdKey, Value: p.value})
	case pkMetadataMatches:
		*out = append(*out, PredicateNode{Kind: "metadata_matches", Key: p.mdKey, Pattern: p.pattern})
	case pkMetadataNumericAtLeast:
		*out = append(*out, PredicateNode{
			Kind:      "metadata_numeric_at_least",
			Key:       p.mdKey,
			Threshold: ptrFloat(p.threshold),
		})
	case pkAnd:
		idxs := make([]int, len(p.children))
		for i, c := range p.children {
			idxs[i] = emit(c, out)
		}
		*out = append(*out, PredicateNode{Kind: "and", Children: idxs})
	case pkOr:
		idxs := make([]int, len(p.children))
		for i, c := range p.children {
			idxs[i] = emit(c, out)
		}
		*out = append(*out, PredicateNode{Kind: "or", Children: idxs})
	case pkNot:
		idx := emit(p.child, out)
		*out = append(*out, PredicateNode{Kind: "not", Child: ptrInt(idx)})
	default:
		panic(fmt.Sprintf("unknown predicate kind: %d", p.kind))
	}
	return len(*out) - 1
}

// PredicateToWire flattens an AST into wire form. Children always
// sit at strictly lower indices than their parents (post-order).
func PredicateToWire(p *Predicate) PredicateWire {
	var nodes []PredicateNode
	root := emit(p, &nodes)
	return PredicateWire{Nodes: nodes, RootIdx: root}
}

// PredicateFromWire is the inverse of PredicateToWire. Returns an
// error on out-of-range indices or unknown node kinds.
func PredicateFromWire(w PredicateWire) (*Predicate, error) {
	built := make([]*Predicate, len(w.Nodes))
	for i, n := range w.Nodes {
		p, err := nodeFromWire(n, built, i)
		if err != nil {
			return nil, err
		}
		built[i] = p
	}
	if w.RootIdx < 0 || w.RootIdx >= len(built) {
		return nil, fmt.Errorf("PredicateFromWire: root_idx %d out of range [0, %d)", w.RootIdx, len(built))
	}
	return built[w.RootIdx], nil
}

func nodeFromWire(n PredicateNode, prior []*Predicate, selfIdx int) (*Predicate, error) {
	checkChild := func(idx int) (*Predicate, error) {
		if idx < 0 || idx >= selfIdx {
			return nil, fmt.Errorf(
				"PredicateFromWire: child index %d not strictly less than self %d",
				idx, selfIdx,
			)
		}
		return prior[idx], nil
	}
	tagKeyFromWire := func(v any) (TagKey, error) {
		// JSON-decoded TagKey arrives as map[string]any.
		m, ok := v.(map[string]any)
		if !ok {
			return TagKey{}, fmt.Errorf("expected TagKey object, got %T", v)
		}
		axis, _ := m["axis"].(string)
		key, _ := m["key"].(string)
		return TagKey{Axis: TaxonomyAxis(axis), Key: key}, nil
	}
	mdKeyFromWire := func(v any) (string, error) {
		s, ok := v.(string)
		if !ok {
			return "", fmt.Errorf("expected metadata key string, got %T", v)
		}
		return s, nil
	}
	switch n.Kind {
	case "exists":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.Exists(k), nil
	case "equals":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.Equals(k, n.Value), nil
	case "numeric_at_least":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		if n.Threshold == nil {
			return nil, errors.New("numeric_at_least: missing threshold")
		}
		return Pred.NumericAtLeast(k, *n.Threshold), nil
	case "numeric_at_most":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		if n.Threshold == nil {
			return nil, errors.New("numeric_at_most: missing threshold")
		}
		return Pred.NumericAtMost(k, *n.Threshold), nil
	case "numeric_in_range":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		if n.Min == nil || n.Max == nil {
			return nil, errors.New("numeric_in_range: missing min or max")
		}
		return Pred.NumericInRange(k, *n.Min, *n.Max), nil
	case "semver_at_least":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.SemverAtLeast(k, n.Version), nil
	case "semver_at_most":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.SemverAtMost(k, n.Version), nil
	case "semver_compatible":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.SemverCompatible(k, n.Version), nil
	case "string_prefix":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.StringPrefix(k, n.Prefix), nil
	case "string_matches":
		k, err := tagKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.StringMatches(k, n.Pattern), nil
	case "metadata_exists":
		k, err := mdKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.MetadataExists(k), nil
	case "metadata_equals":
		k, err := mdKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.MetadataEquals(k, n.Value), nil
	case "metadata_matches":
		k, err := mdKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		return Pred.MetadataMatches(k, n.Pattern), nil
	case "metadata_numeric_at_least":
		k, err := mdKeyFromWire(n.Key)
		if err != nil {
			return nil, err
		}
		if n.Threshold == nil {
			return nil, errors.New("metadata_numeric_at_least: missing threshold")
		}
		return Pred.MetadataNumericAtLeast(k, *n.Threshold), nil
	case "and":
		out := make([]*Predicate, 0, len(n.Children))
		for _, idx := range n.Children {
			c, err := checkChild(idx)
			if err != nil {
				return nil, err
			}
			out = append(out, c)
		}
		return Pred.And(out...), nil
	case "or":
		out := make([]*Predicate, 0, len(n.Children))
		for _, idx := range n.Children {
			c, err := checkChild(idx)
			if err != nil {
				return nil, err
			}
			out = append(out, c)
		}
		return Pred.Or(out...), nil
	case "not":
		if n.Child == nil {
			return nil, errors.New("not: missing child")
		}
		c, err := checkChild(*n.Child)
		if err != nil {
			return nil, err
		}
		return Pred.Not(c), nil
	default:
		return nil, fmt.Errorf("unknown predicate kind: %q", n.Kind)
	}
}

// nRPC envelope helpers ----------------------------------------------------

// RPCWhereHeader is the header the substrate uses to carry a
// predicate over nRPC.
const RPCWhereHeader = "cyberdeck-where"

// PredicateToRPCHeader encodes a predicate to the request-header
// value (canonical JSON-encoded PredicateWire).
func PredicateToRPCHeader(p *Predicate) (string, error) {
	w := PredicateToWire(p)
	b, err := json.Marshal(w)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// PredicateFromRPCHeader decodes a `cyberdeck-where` header value
// into a predicate AST.
func PredicateFromRPCHeader(value string) (*Predicate, error) {
	var w PredicateWire
	if err := json.Unmarshal([]byte(value), &w); err != nil {
		return nil, err
	}
	return PredicateFromWire(w)
}

// ============================================================================
// CapabilitySet diff — wire-format input, sorted output.
// ============================================================================

// CapabilitySetWire is the wire-format capability shape — string
// tags + str→str metadata.
type CapabilitySetWire struct {
	Tags     []string          `json:"tags"`
	Metadata map[string]string `json:"metadata"`
}

// MetadataChangeKind discriminates the change variant. The wire
// `kind` strings ("added", "removed", "updated") are stable.
type MetadataChangeKind string

const (
	MetadataChangeAdded   MetadataChangeKind = "added"
	MetadataChangeRemoved MetadataChangeKind = "removed"
	MetadataChangeUpdated MetadataChangeKind = "updated"
)

// MetadataChange captures a per-key add / remove / update. The
// substrate's `MetadataChange` enum maps onto this shape; unused
// fields are omitted from JSON via `omitempty`.
type MetadataChange struct {
	Kind      MetadataChangeKind `json:"kind"`
	Key       string             `json:"key"`
	Value     string             `json:"value,omitempty"`
	PrevValue string             `json:"prev_value,omitempty"`
	NewValue  string             `json:"new_value,omitempty"`
}

// CapabilitySetDiff is the output of DiffCapabilities. Pinned by
// the `capability_set_diff.json` cross-binding fixture.
type CapabilitySetDiff struct {
	AddedTags       []string         `json:"added_tags"`
	RemovedTags     []string         `json:"removed_tags"`
	MetadataChanges []MetadataChange `json:"metadata_changes"`
}

// DiffCapabilities computes `curr.diff(prev)`. Tag arrays are
// sorted by wire string; metadata changes sorted by key (BTreeMap
// semantics in the substrate).
//
// Semantics: a key rename surfaces as Removed + Added (NOT Updated).
// Only a value change for the same key is Updated.
func DiffCapabilities(prev, curr CapabilitySetWire) CapabilitySetDiff {
	prevTags := make(map[string]struct{}, len(prev.Tags))
	for _, t := range prev.Tags {
		prevTags[t] = struct{}{}
	}
	currTags := make(map[string]struct{}, len(curr.Tags))
	for _, t := range curr.Tags {
		currTags[t] = struct{}{}
	}
	added := make([]string, 0)
	for t := range currTags {
		if _, ok := prevTags[t]; !ok {
			added = append(added, t)
		}
	}
	removed := make([]string, 0)
	for t := range prevTags {
		if _, ok := currTags[t]; !ok {
			removed = append(removed, t)
		}
	}
	sort.Strings(added)
	sort.Strings(removed)

	keys := make(map[string]struct{}, len(prev.Metadata)+len(curr.Metadata))
	for k := range prev.Metadata {
		keys[k] = struct{}{}
	}
	for k := range curr.Metadata {
		keys[k] = struct{}{}
	}
	sortedKeys := make([]string, 0, len(keys))
	for k := range keys {
		sortedKeys = append(sortedKeys, k)
	}
	sort.Strings(sortedKeys)

	changes := make([]MetadataChange, 0)
	for _, k := range sortedKeys {
		pv, inPrev := prev.Metadata[k]
		nv, inCurr := curr.Metadata[k]
		switch {
		case inPrev && inCurr:
			if pv != nv {
				changes = append(changes, MetadataChange{
					Kind:      MetadataChangeUpdated,
					Key:       k,
					PrevValue: pv,
					NewValue:  nv,
				})
			}
		case inCurr:
			changes = append(changes, MetadataChange{
				Kind:  MetadataChangeAdded,
				Key:   k,
				Value: nv,
			})
		case inPrev:
			changes = append(changes, MetadataChange{
				Kind:      MetadataChangeRemoved,
				Key:       k,
				PrevValue: pv,
			})
		}
	}

	return CapabilitySetDiff{
		AddedTags:       added,
		RemovedTags:     removed,
		MetadataChanges: changes,
	}
}

// ============================================================================
// Chain composition helpers
// ============================================================================

// EmptyCapabilities returns an empty wire-format capability set.
func EmptyCapabilities() CapabilitySetWire {
	return CapabilitySetWire{Tags: nil, Metadata: map[string]string{}}
}

func appendUnique(tags []string, t string) []string {
	for _, existing := range tags {
		if existing == t {
			return tags
		}
	}
	return append(tags, t)
}

// RequireTag adds an axis-tag (no value) to the wire shape.
// Idempotent; no-op if the tag is already present.
func RequireTag(caps CapabilitySetWire, axis TaxonomyAxis, key string) (CapabilitySetWire, error) {
	if key == "" {
		return CapabilitySetWire{}, errors.New("RequireTag: key must be non-empty")
	}
	wire := NewAxisPresentTag(axis, key).String()
	out := CapabilitySetWire{
		Tags:     appendUnique(append([]string(nil), caps.Tags...), wire),
		Metadata: copyMetadata(caps.Metadata),
	}
	return out, nil
}

// RequireAxisValue adds `<axis>.<key><sep><value>` to the wire shape.
// Idempotent for the exact (axis, key, value, separator) tuple.
func RequireAxisValue(
	caps CapabilitySetWire,
	axis TaxonomyAxis,
	key, value string,
	sep AxisSeparator,
) (CapabilitySetWire, error) {
	if key == "" {
		return CapabilitySetWire{}, errors.New("RequireAxisValue: key must be non-empty")
	}
	if value == "" {
		return CapabilitySetWire{}, errors.New("RequireAxisValue: value must be non-empty")
	}
	wire := NewAxisValueTag(axis, key, value, sep).String()
	out := CapabilitySetWire{
		Tags:     appendUnique(append([]string(nil), caps.Tags...), wire),
		Metadata: copyMetadata(caps.Metadata),
	}
	return out, nil
}

// WithMetadata sets / overwrites a metadata entry.
func WithMetadata(caps CapabilitySetWire, key, value string) (CapabilitySetWire, error) {
	if key == "" {
		return CapabilitySetWire{}, errors.New("WithMetadata: key must be non-empty")
	}
	md := copyMetadata(caps.Metadata)
	md[key] = value
	return CapabilitySetWire{
		Tags:     append([]string(nil), caps.Tags...),
		Metadata: md,
	}, nil
}

func copyMetadata(m map[string]string) map[string]string {
	out := make(map[string]string, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}

// ============================================================================
// StandardPlacement — config object + builder.
// ============================================================================

// StandardPlacement is the JSON-serializable configuration for the
// substrate's placement filter. All fields optional; an empty
// config matches every node.
type StandardPlacement struct {
	RequireTags     []string          `json:"require_tags,omitempty"`
	ForbidTags      []string          `json:"forbid_tags,omitempty"`
	RequireMetadata map[string]string `json:"require_metadata,omitempty"`
	Predicate       *PredicateWire    `json:"predicate,omitempty"`
	Limit           *int              `json:"limit,omitempty"`
	CustomFilterID  string            `json:"custom_filter_id,omitempty"`
}

// StandardPlacementBuilder is the fluent builder for
// StandardPlacement. Returned configs are deep-copied so subsequent
// mutations on the builder don't bleed.
type StandardPlacementBuilder struct {
	requireTags     []string
	forbidTags      []string
	requireMetadata map[string]string
	predicate       *PredicateWire
	limit           *int
	customFilterID  string
}

// NewStandardPlacementBuilder constructs an empty builder.
func NewStandardPlacementBuilder() *StandardPlacementBuilder {
	return &StandardPlacementBuilder{requireMetadata: map[string]string{}}
}

func (b *StandardPlacementBuilder) RequireTag(axis TaxonomyAxis, key string) *StandardPlacementBuilder {
	b.requireTags = append(b.requireTags, NewAxisPresentTag(axis, key).String())
	return b
}

func (b *StandardPlacementBuilder) RequireAxisValue(
	axis TaxonomyAxis, key, value string, sep AxisSeparator,
) *StandardPlacementBuilder {
	b.requireTags = append(b.requireTags, NewAxisValueTag(axis, key, value, sep).String())
	return b
}

func (b *StandardPlacementBuilder) ForbidTag(axis TaxonomyAxis, key string) *StandardPlacementBuilder {
	b.forbidTags = append(b.forbidTags, NewAxisPresentTag(axis, key).String())
	return b
}

func (b *StandardPlacementBuilder) RequireMetadata(key, value string) *StandardPlacementBuilder {
	b.requireMetadata[key] = value
	return b
}

// WithPredicate accepts either an AST or a pre-built PredicateWire.
func (b *StandardPlacementBuilder) WithPredicate(p *Predicate) *StandardPlacementBuilder {
	w := PredicateToWire(p)
	b.predicate = &w
	return b
}

// WithPredicateWire accepts a pre-built wire form (e.g. one
// deserialized from somewhere else).
func (b *StandardPlacementBuilder) WithPredicateWire(w PredicateWire) *StandardPlacementBuilder {
	clone := PredicateWire{
		Nodes:   append([]PredicateNode(nil), w.Nodes...),
		RootIdx: w.RootIdx,
	}
	b.predicate = &clone
	return b
}

// WithLimit caps the candidate count. n must be non-negative.
func (b *StandardPlacementBuilder) WithLimit(n int) (*StandardPlacementBuilder, error) {
	if n < 0 {
		return nil, errors.New("WithLimit: n must be non-negative")
	}
	v := n
	b.limit = &v
	return b, nil
}

func (b *StandardPlacementBuilder) WithCustomFilterID(id string) (*StandardPlacementBuilder, error) {
	if id == "" {
		return nil, errors.New("WithCustomFilterID: id must be non-empty")
	}
	b.customFilterID = id
	return b, nil
}

// Build produces the immutable StandardPlacement config.
func (b *StandardPlacementBuilder) Build() StandardPlacement {
	out := StandardPlacement{}
	if len(b.requireTags) > 0 {
		out.RequireTags = append([]string(nil), b.requireTags...)
	}
	if len(b.forbidTags) > 0 {
		out.ForbidTags = append([]string(nil), b.forbidTags...)
	}
	if len(b.requireMetadata) > 0 {
		out.RequireMetadata = copyMetadata(b.requireMetadata)
	}
	if b.predicate != nil {
		clone := PredicateWire{
			Nodes:   append([]PredicateNode(nil), b.predicate.Nodes...),
			RootIdx: b.predicate.RootIdx,
		}
		out.Predicate = &clone
	}
	if b.limit != nil {
		v := *b.limit
		out.Limit = &v
	}
	if b.customFilterID != "" {
		out.CustomFilterID = b.customFilterID
	}
	return out
}

// ============================================================================
// Custom placement-filter callback
// ============================================================================

// PlacementCandidate is the per-candidate context passed to a
// custom placement filter.
type PlacementCandidate struct {
	NodeID   uint64
	Tags     []string
	Metadata map[string]string
}

// PlacementFilterFn is a synchronous predicate: true to keep, false
// to drop. Run in the placement hot path — keep it tight, no I/O.
type PlacementFilterFn func(PlacementCandidate) bool

// RegisteredPlacementFilter is the registration record returned by
// PlacementFilterFromFn. The runtime registers (id, fn) pairs;
// StandardPlacement.CustomFilterID references the id.
type RegisteredPlacementFilter struct {
	ID string
	Fn PlacementFilterFn
}

var placementFilterCounter atomic.Uint64

// PlacementFilterFromFn wraps a user predicate as a registered
// placement filter. If `explicitID` is empty, an auto-incremented
// id is assigned.
func PlacementFilterFromFn(fn PlacementFilterFn, explicitID string) RegisteredPlacementFilter {
	id := explicitID
	if id == "" {
		n := placementFilterCounter.Add(1)
		id = fmt.Sprintf("pf-%d", n)
	}
	return RegisteredPlacementFilter{ID: id, Fn: fn}
}

// ============================================================================
// Predicate evaluation — pure local evaluator over (tags, metadata).
//
// Mirrors the substrate's `Predicate::evaluate_unplanned`: composite
// recursion in declaration order with short-circuit semantics. The
// planned variant in the substrate reorders And/Or children by static
// cost; the boolean answer is invariant. Pinned across bindings by
// `tests/cross_lang_capability/predicate_eval.json`.
// ============================================================================

type semverTriple [3]uint64

func parseSemverGo(s string) (semverTriple, bool) {
	// Drop pre-release / build suffix.
	dash := strings.IndexByte(s, '-')
	plus := strings.IndexByte(s, '+')
	var core string
	switch {
	case dash >= 0 && plus >= 0:
		if dash < plus {
			core = s[:dash]
		} else {
			core = s[:plus]
		}
	case dash >= 0:
		core = s[:dash]
	case plus >= 0:
		core = s[:plus]
	default:
		core = s
	}
	parts := strings.Split(core, ".")
	if len(parts) == 0 || len(parts) > 3 {
		return semverTriple{}, false
	}
	parsePart := func(p string) (uint64, bool) {
		p = strings.TrimSpace(p)
		if p == "" {
			return 0, false
		}
		var n uint64
		for _, c := range p {
			if c < '0' || c > '9' {
				return 0, false
			}
			n = n*10 + uint64(c-'0')
		}
		return n, true
	}
	major, ok := parsePart(parts[0])
	if !ok {
		return semverTriple{}, false
	}
	var minor, patch uint64
	if len(parts) > 1 {
		var ok2 bool
		minor, ok2 = parsePart(parts[1])
		if !ok2 {
			return semverTriple{}, false
		}
	}
	if len(parts) > 2 {
		var ok3 bool
		patch, ok3 = parsePart(parts[2])
		if !ok3 {
			return semverTriple{}, false
		}
	}
	return semverTriple{major, minor, patch}, true
}

func semverCmp(a, b semverTriple) int {
	for i := 0; i < 3; i++ {
		if a[i] < b[i] {
			return -1
		}
		if a[i] > b[i] {
			return 1
		}
	}
	return 0
}

func semverCompatibleGo(lhs, rhs semverTriple) bool {
	if semverCmp(lhs, rhs) < 0 {
		return false
	}
	if rhs[0] == 0 {
		return rhs[1] == lhs[1]
	}
	return rhs[0] == lhs[0]
}

// axisTagValue returns the matched value for an axis-keyed tag, or
// the empty string for AxisPresent. Returns ("", false) when no tag
// matches.
func axisTagValue(tags []string, key TagKey) (string, bool) {
	prefix := string(key.Axis) + "." + key.Key
	for _, wire := range tags {
		if wire == prefix {
			return "", true
		}
		if len(wire) <= len(prefix) || !strings.HasPrefix(wire, prefix) {
			continue
		}
		sep := wire[len(prefix)]
		if sep == '=' || sep == ':' {
			return wire[len(prefix)+1:], true
		}
	}
	return "", false
}

func isNumericLiteral(s string) bool {
	if s == "" {
		return false
	}
	i := 0
	if s[0] == '-' {
		i = 1
		if len(s) == 1 {
			return false
		}
	}
	seenDot := false
	digit := false
	for ; i < len(s); i++ {
		c := s[i]
		if c >= '0' && c <= '9' {
			digit = true
			continue
		}
		if c == '.' && !seenDot {
			seenDot = true
			continue
		}
		return false
	}
	return digit
}

func parseFloat(s string) (float64, bool) {
	if !isNumericLiteral(s) {
		return 0, false
	}
	var n float64
	if _, err := fmt.Sscanf(s, "%f", &n); err != nil {
		return 0, false
	}
	return n, true
}

func evalLeaf(p *Predicate, tags []string, metadata map[string]string) bool {
	switch p.kind {
	case pkExists:
		_, ok := axisTagValue(tags, p.key)
		return ok
	case pkEquals:
		v, ok := axisTagValue(tags, p.key)
		return ok && v == p.value
	case pkNumericAtLeast:
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		n, ok := parseFloat(v)
		return ok && n >= p.threshold
	case pkNumericAtMost:
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		n, ok := parseFloat(v)
		return ok && n <= p.threshold
	case pkNumericInRange:
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		n, ok := parseFloat(v)
		return ok && n >= p.min && n <= p.max
	case pkSemverAtLeast:
		rhs, ok := parseSemverGo(p.version)
		if !ok {
			return false
		}
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		lhs, ok := parseSemverGo(v)
		return ok && semverCmp(lhs, rhs) >= 0
	case pkSemverAtMost:
		rhs, ok := parseSemverGo(p.version)
		if !ok {
			return false
		}
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		lhs, ok := parseSemverGo(v)
		return ok && semverCmp(lhs, rhs) <= 0
	case pkSemverCompatible:
		rhs, ok := parseSemverGo(p.version)
		if !ok {
			return false
		}
		v, ok := axisTagValue(tags, p.key)
		if !ok {
			return false
		}
		lhs, ok := parseSemverGo(v)
		return ok && semverCompatibleGo(lhs, rhs)
	case pkStringPrefix:
		v, ok := axisTagValue(tags, p.key)
		return ok && strings.HasPrefix(v, p.prefix)
	case pkStringMatches:
		v, ok := axisTagValue(tags, p.key)
		return ok && strings.Contains(v, p.pattern)
	case pkMetadataExists:
		_, ok := metadata[p.mdKey]
		return ok
	case pkMetadataEquals:
		v, ok := metadata[p.mdKey]
		return ok && v == p.value
	case pkMetadataMatches:
		v, ok := metadata[p.mdKey]
		return ok && strings.Contains(v, p.pattern)
	case pkMetadataNumericAtLeast:
		v, ok := metadata[p.mdKey]
		if !ok {
			return false
		}
		n, ok := parseFloat(v)
		return ok && n >= p.threshold
	default:
		panic(fmt.Sprintf("evalLeaf: composite predicate kind=%d routed through leaf evaluator", p.kind))
	}
}

// EvaluatePredicate evaluates a Predicate against a wire-format
// (tags, metadata) context. Mirrors the substrate's
// `Predicate::evaluate_unplanned`; children of And / Or evaluate in
// declaration order with short-circuit semantics. Pinned across
// bindings by `predicate_eval.json`.
func EvaluatePredicate(p *Predicate, tags []string, metadata map[string]string) bool {
	switch p.kind {
	case pkAnd:
		for _, c := range p.children {
			if !EvaluatePredicate(c, tags, metadata) {
				return false
			}
		}
		return true
	case pkOr:
		for _, c := range p.children {
			if EvaluatePredicate(c, tags, metadata) {
				return true
			}
		}
		return false
	case pkNot:
		return !EvaluatePredicate(p.child, tags, metadata)
	default:
		return evalLeaf(p, tags, metadata)
	}
}

// ============================================================================
// Predicate trace evaluator — Phase 9d slice. Mirrors the substrate's
// `Predicate::evaluate_with_trace`: cost-ordered, short-circuiting,
// drops siblings that didn't run from the trace. Pinned across
// bindings by `predicate_trace.json`.
// ============================================================================

// ClauseTrace is the wire-format trace tree. Mirrors the substrate's
// `ClauseTrace`. JSON tags pin `label` / `result` / `children` so the
// shape round-trips with the cross-binding fixture.
type ClauseTrace struct {
	Label    string        `json:"label"`
	Result   bool          `json:"result"`
	Children []ClauseTrace `json:"children"`
}

func predStaticCost(p *Predicate) uint32 {
	switch p.kind {
	case pkMetadataExists:
		return 10
	case pkMetadataEquals:
		return 11
	case pkExists:
		return 20
	case pkEquals:
		return 21
	case pkMetadataNumericAtLeast:
		return 25
	case pkNumericAtLeast, pkNumericAtMost, pkNumericInRange:
		return 30
	case pkStringPrefix:
		return 40
	case pkMetadataMatches:
		return 45
	case pkStringMatches:
		return 50
	case pkSemverAtLeast, pkSemverAtMost, pkSemverCompatible:
		return 60
	case pkAnd, pkOr:
		var s uint64
		for _, c := range p.children {
			s += uint64(predStaticCost(c))
			if s > 0xFFFFFFFF {
				return 0xFFFFFFFF
			}
		}
		return uint32(s)
	case pkNot:
		return predStaticCost(p.child)
	}
	return 0
}

func formatFloat(n float64) string {
	// Match Rust's `{}` Display: integers print without decimals.
	if n == float64(int64(n)) && n >= -1e16 && n <= 1e16 {
		return fmt.Sprintf("%d", int64(n))
	}
	return fmt.Sprintf("%g", n)
}

func rustDbgString(s string) string {
	// Match Rust's `{:?}` debug-format for &str: encoded JSON string
	// is byte-equal for plain ASCII strings.
	b, _ := json.Marshal(s)
	return string(b)
}

func tagKeyDisplay(k TagKey) string {
	return string(k.Axis) + "." + k.Key
}

func predDebugLabel(p *Predicate) string {
	switch p.kind {
	case pkExists:
		return "Exists(" + tagKeyDisplay(p.key) + ")"
	case pkEquals:
		return "Equals(" + tagKeyDisplay(p.key) + "=" + p.value + ")"
	case pkNumericAtLeast:
		return "NumericAtLeast(" + tagKeyDisplay(p.key) + " >= " + formatFloat(p.threshold) + ")"
	case pkNumericAtMost:
		return "NumericAtMost(" + tagKeyDisplay(p.key) + " <= " + formatFloat(p.threshold) + ")"
	case pkNumericInRange:
		return "NumericInRange(" + tagKeyDisplay(p.key) + " in [" +
			formatFloat(p.min) + ", " + formatFloat(p.max) + "])"
	case pkSemverAtLeast:
		return "SemverAtLeast(" + tagKeyDisplay(p.key) + " >= " + p.version + ")"
	case pkSemverAtMost:
		return "SemverAtMost(" + tagKeyDisplay(p.key) + " <= " + p.version + ")"
	case pkSemverCompatible:
		return "SemverCompatible(" + tagKeyDisplay(p.key) + " ~= " + p.version + ")"
	case pkStringPrefix:
		return "StringPrefix(" + tagKeyDisplay(p.key) + " starts with " + rustDbgString(p.prefix) + ")"
	case pkStringMatches:
		return "StringMatches(" + tagKeyDisplay(p.key) + " contains " + rustDbgString(p.pattern) + ")"
	case pkMetadataExists:
		return "MetadataExists(" + p.mdKey + ")"
	case pkMetadataEquals:
		return "MetadataEquals(" + p.mdKey + "=" + p.value + ")"
	case pkMetadataMatches:
		return "MetadataMatches(" + p.mdKey + " contains " + rustDbgString(p.pattern) + ")"
	case pkMetadataNumericAtLeast:
		return "MetadataNumericAtLeast(" + p.mdKey + " >= " + formatFloat(p.threshold) + ")"
	case pkAnd:
		return fmt.Sprintf("And(%d clauses)", len(p.children))
	case pkOr:
		return fmt.Sprintf("Or(%d clauses)", len(p.children))
	case pkNot:
		return "Not"
	}
	return ""
}

// planChildren returns the children sorted by static cost (ascending),
// preserving declaration order for ties (stable sort).
func planChildren(children []*Predicate) []*Predicate {
	type indexed struct {
		c    *Predicate
		i    int
		cost uint32
	}
	idx := make([]indexed, len(children))
	for i, c := range children {
		idx[i] = indexed{c: c, i: i, cost: predStaticCost(c)}
	}
	sort.SliceStable(idx, func(a, b int) bool {
		return idx[a].cost < idx[b].cost
	})
	out := make([]*Predicate, len(idx))
	for i, x := range idx {
		out[i] = x.c
	}
	return out
}

// EvaluatePredicateWithTrace evaluates a predicate against (tags,
// metadata) and produces a trace tree. Mirrors the substrate's
// `Predicate::evaluate_with_trace`: cost-ordered, short-circuiting,
// drops siblings that didn't run.
func EvaluatePredicateWithTrace(
	p *Predicate, tags []string, metadata map[string]string,
) (bool, ClauseTrace) {
	label := predDebugLabel(p)
	switch p.kind {
	case pkAnd:
		ordered := planChildren(p.children)
		traces := make([]ClauseTrace, 0, len(ordered))
		result := true
		for _, c := range ordered {
			r, t := EvaluatePredicateWithTrace(c, tags, metadata)
			traces = append(traces, t)
			if !r {
				result = false
				break
			}
		}
		return result, ClauseTrace{Label: label, Result: result, Children: traces}
	case pkOr:
		ordered := planChildren(p.children)
		traces := make([]ClauseTrace, 0, len(ordered))
		result := false
		for _, c := range ordered {
			r, t := EvaluatePredicateWithTrace(c, tags, metadata)
			traces = append(traces, t)
			if r {
				result = true
				break
			}
		}
		return result, ClauseTrace{Label: label, Result: result, Children: traces}
	case pkNot:
		r, t := EvaluatePredicateWithTrace(p.child, tags, metadata)
		return !r, ClauseTrace{Label: label, Result: !r, Children: []ClauseTrace{t}}
	}
	r := evalLeaf(p, tags, metadata)
	return r, ClauseTrace{Label: label, Result: r, Children: []ClauseTrace{}}
}
