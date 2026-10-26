package rules

import (
	"fmt"
	"os"
	"strings"

	"google.golang.org/protobuf/encoding/protowire"
)

// loadGeosite reads the GeoSiteList protobuf used by v2ray/Xray geosite.dat
// without importing the full v2ray core package. Only the domain types useful
// for DNS classification are indexed: RootDomain and Full. Regex and keyword
// entries are deliberately ignored because they cannot be represented safely
// by the suffix trie.
func loadGeosite(path, domesticName, foreignName string, domestic, foreign *domainTrie) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	wantDomestic := strings.EqualFold(domesticName, "")
	wantForeign := strings.EqualFold(foreignName, "")
	for len(b) > 0 {
		num, typ, n := protowire.ConsumeTag(b)
		if n < 0 {
			return fmt.Errorf("geosite: invalid list tag: %v", protowire.ParseError(n))
		}
		b = b[n:]
		if num == 1 && typ == protowire.BytesType {
			entry, used := protowire.ConsumeBytes(b)
			if used < 0 {
				return fmt.Errorf("geosite: invalid site entry: %v", protowire.ParseError(used))
			}
			if err := loadGeoSiteEntry(entry, domesticName, foreignName, domestic, foreign, &wantDomestic, &wantForeign); err != nil {
				return err
			}
			b = b[used:]
			continue
		}
		used := protowire.ConsumeFieldValue(num, typ, b)
		if used < 0 {
			return fmt.Errorf("geosite: invalid field: %v", protowire.ParseError(used))
		}
		b = b[used:]
	}
	return nil
}

func loadGeoSiteEntry(b []byte, domesticName, foreignName string, domestic, foreign *domainTrie, wantDomestic, wantForeign *bool) error {
	var name string
	var domains [][]byte
	for len(b) > 0 {
		num, typ, n := protowire.ConsumeTag(b)
		if n < 0 {
			return fmt.Errorf("geosite: invalid site tag: %v", protowire.ParseError(n))
		}
		b = b[n:]
		switch {
		case num == 1 && typ == protowire.BytesType:
			v, used := protowire.ConsumeBytes(b)
			if used < 0 {
				return fmt.Errorf("geosite: invalid country code: %v", protowire.ParseError(used))
			}
			name = string(v)
			b = b[used:]
		case num == 2 && typ == protowire.BytesType:
			v, used := protowire.ConsumeBytes(b)
			if used < 0 {
				return fmt.Errorf("geosite: invalid domain: %v", protowire.ParseError(used))
			}
			domains = append(domains, append([]byte(nil), v...))
			b = b[used:]
		default:
			used := protowire.ConsumeFieldValue(num, typ, b)
			if used < 0 {
				return fmt.Errorf("geosite: invalid site field: %v", protowire.ParseError(used))
			}
			b = b[used:]
		}
	}

	toDomestic := strings.EqualFold(name, domesticName) || (*wantDomestic && name == "")
	toForeign := strings.EqualFold(name, foreignName) || (*wantForeign && name == "")
	if !toDomestic && !toForeign {
		return nil
	}
	for _, raw := range domains {
		typ, value, ok, err := parseGeoSiteDomain(raw)
		if err != nil {
			return err
		}
		if !ok {
			continue
		}
		value, err = normalize(value)
		if err != nil {
			continue
		}
		if toDomestic {
			domestic.add(value, typ == 2, typ == 3)
		}
		if toForeign {
			foreign.add(value, typ == 2, typ == 3)
		}
	}
	return nil
}

func parseGeoSiteDomain(b []byte) (typ int, value string, ok bool, err error) {
	var typeValue uint64
	for len(b) > 0 {
		num, wire, n := protowire.ConsumeTag(b)
		if n < 0 {
			return 0, "", false, protowire.ParseError(n)
		}
		b = b[n:]
		switch {
		case num == 1 && wire == protowire.VarintType:
			v, used := protowire.ConsumeVarint(b)
			if used < 0 {
				return 0, "", false, protowire.ParseError(used)
			}
			typeValue = v
			b = b[used:]
		case num == 2 && wire == protowire.BytesType:
			v, used := protowire.ConsumeBytes(b)
			if used < 0 {
				return 0, "", false, protowire.ParseError(used)
			}
			value = string(v)
			b = b[used:]
		default:
			used := protowire.ConsumeFieldValue(num, wire, b)
			if used < 0 {
				return 0, "", false, protowire.ParseError(used)
			}
			b = b[used:]
		}
	}
	return int(typeValue), value, value != "", nil
}
