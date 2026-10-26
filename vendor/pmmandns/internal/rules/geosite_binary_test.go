package rules

import (
	"os"
	"path/filepath"
	"testing"

	"google.golang.org/protobuf/encoding/protowire"
)

func TestLoadGeositeBinary(t *testing.T) {
	domain := protowire.AppendTag(nil, 1, protowire.VarintType)
	domain = protowire.AppendVarint(domain, 2) // RootDomain
	domain = protowire.AppendTag(domain, 2, protowire.BytesType)
	domain = protowire.AppendBytes(domain, []byte("example.com"))
	full := protowire.AppendTag(nil, 1, protowire.VarintType)
	full = protowire.AppendVarint(full, 3) // Full
	full = protowire.AppendTag(full, 2, protowire.BytesType)
	full = protowire.AppendBytes(full, []byte("only.example"))
	site := protowire.AppendTag(nil, 1, protowire.BytesType)
	site = protowire.AppendBytes(site, []byte("cn"))
	site = protowire.AppendTag(site, 2, protowire.BytesType)
	site = protowire.AppendBytes(site, domain)
	site = protowire.AppendTag(site, 2, protowire.BytesType)
	site = protowire.AppendBytes(site, full)
	list := protowire.AppendTag(nil, 1, protowire.BytesType)
	list = protowire.AppendBytes(list, site)

	path := filepath.Join(t.TempDir(), "geosite.dat")
	if err := os.WriteFile(path, list, 0600); err != nil {
		t.Fatal(err)
	}
	m, err := NewWithGeosite(path, "cn", "foreign", nil, nil, Foreign)
	if err != nil {
		t.Fatal(err)
	}
	if m.Match("www.example.com") != Domestic || m.Match("only.example") != Domestic || m.Match("www.only.example") == Domestic {
		t.Fatal("binary geosite rules were not indexed correctly")
	}
}
