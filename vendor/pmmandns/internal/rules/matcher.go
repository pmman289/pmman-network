package rules

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"golang.org/x/net/idna"
)

type Group int

const (
	Domestic Group = iota
	Foreign
)

// Matcher keeps rules in reversed-label tries. A lookup only walks the labels
// in the query name and never scans the rule files.
type Matcher struct {
	domestic *domainTrie
	foreign  *domainTrie
	def      Group
}

type trieNode struct {
	children map[string]*trieNode
	suffix   bool // domain:foo.example matches foo.example and subdomains.
	exact    bool // full:foo.example matches only foo.example.
}

type domainTrie struct{ root *trieNode }

func newDomainTrie() *domainTrie {
	return &domainTrie{root: &trieNode{children: make(map[string]*trieNode)}}
}

func New(domesticFiles, foreignFiles []string, def Group) (*Matcher, error) {
	return NewWithGeosite("", "cn", "geolocation-!cn", domesticFiles, foreignFiles, def)
}

func NewWithGeosite(geositeFile, domesticSite, foreignSite string, domesticFiles, foreignFiles []string, def Group) (*Matcher, error) {
	m := &Matcher{domestic: newDomainTrie(), foreign: newDomainTrie(), def: def}
	if geositeFile != "" {
		if err := loadGeosite(geositeFile, domesticSite, foreignSite, m.domestic, m.foreign); err != nil {
			return nil, err
		}
	}
	for _, path := range domesticFiles {
		if err := load(path, m.domestic); err != nil {
			return nil, err
		}
	}
	for _, path := range foreignFiles {
		if err := load(path, m.foreign); err != nil {
			return nil, err
		}
	}
	return m, nil
}

func normalize(s string) (string, error) {
	s = strings.TrimSuffix(strings.ToLower(strings.TrimSpace(s)), ".")
	s = strings.TrimPrefix(s, "*.")
	if s == "" {
		return "", fmt.Errorf("empty domain")
	}
	return idna.Lookup.ToASCII(s)
}

// load accepts both the project's original one-domain-per-line format and the
// plain text exported by v2fly/domain-list-community.
func load(path string, dst *domainTrie) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()

	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 4096), 1024*1024)
	for s.Scan() {
		line := strings.TrimSpace(strings.SplitN(s.Text(), "#", 2)[0])
		if line == "" || strings.HasPrefix(line, "include:") {
			continue
		}
		typ, value, ok := parseRule(line)
		if !ok {
			// keyword/regexp rules cannot be represented by a suffix trie. The
			// official cn/geolocation lists contain a small number of these;
			// skipping them is safer than treating a pattern as a hostname.
			continue
		}
		domain, err := normalize(value)
		if err != nil {
			// Public geosite exports can contain labels that are meaningful to
			// the originating matcher but not valid IDNA hostnames. Ignore them
			// instead of making the DNS daemon fail to start.
			continue
		}
		dst.add(domain, typ == "domain", typ == "full")
	}
	return s.Err()
}

func parseRule(line string) (typ, value string, ok bool) {
	typ = "domain"
	value = line
	if i := strings.IndexByte(line, ':'); i >= 0 {
		candidate := line[:i]
		switch candidate {
		case "domain", "full":
			typ, value = candidate, line[i+1:]
		case "keyword", "regexp", "include":
			return "", "", false
		}
	}
	// Exported geosite lines may have attributes after the value, e.g.
	// domain:example.com:@cn. Attributes are not relevant to this proxy.
	fields := strings.FieldsFunc(value, func(r rune) bool { return r == ':' || r == ' ' || r == '\t' })
	if len(fields) == 0 {
		return "", "", false
	}
	return typ, fields[0], true
}

func (t *domainTrie) add(name string, suffix, exact bool) {
	n := t.root
	parts := strings.Split(name, ".")
	for i := len(parts) - 1; i >= 0; i-- {
		child := n.children[parts[i]]
		if child == nil {
			child = &trieNode{children: make(map[string]*trieNode)}
			n.children[parts[i]] = child
		}
		n = child
	}
	n.suffix = n.suffix || suffix
	n.exact = n.exact || exact
}

func (t *domainTrie) match(name string) bool {
	parts := strings.Split(name, ".")
	n := t.root
	for i := len(parts) - 1; i >= 0; i-- {
		n = n.children[parts[i]]
		if n == nil {
			return false
		}
		if n.suffix || (i == 0 && n.exact) {
			return true
		}
	}
	return false
}

func (m *Matcher) Match(name string) Group {
	n, err := normalize(name)
	if err != nil {
		return m.def
	}
	if m.domestic.match(n) {
		return Domestic
	}
	if m.foreign.match(n) {
		return Foreign
	}
	return m.def
}
