package config

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
)

type Config struct {
	Listen            []string `json:"listen"`
	DomesticUpstreams []string `json:"domestic_upstreams"`
	ForeignUpstreams  []string `json:"foreign_upstreams"`
	DomesticRules     []string `json:"domestic_rules"`
	ForeignRules      []string `json:"foreign_rules"`
	GeositeFile       string   `json:"geosite_file"`
	GeositeDomestic   string   `json:"geosite_domestic"`
	GeositeForeign    string   `json:"geosite_foreign"`
	DefaultGroup      string   `json:"default_group"`
	TimeoutMS         int      `json:"timeout_ms"`
	CacheSize         int      `json:"cache_size"`
	MaxTTL            uint32   `json:"max_ttl"`
	NegativeTTL       uint32   `json:"negative_ttl"`
	LocalUpstream     string   `json:"local_upstream"`
	LocalSuffixes     []string `json:"local_suffixes"`
	DN42Upstreams     []string `json:"dn42_upstreams"`
}

func Load(path string) (Config, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return Config{}, err
	}
	c := Config{DefaultGroup: "foreign", TimeoutMS: 1200, CacheSize: 4096, MaxTTL: 86400, NegativeTTL: 30}
	if err := json.Unmarshal(b, &c); err != nil {
		return Config{}, err
	}
	if len(c.Listen) == 0 || len(c.DomesticUpstreams) == 0 || len(c.ForeignUpstreams) == 0 {
		return Config{}, fmt.Errorf("listen and both upstream groups are required")
	}
	if c.DefaultGroup != "domestic" && c.DefaultGroup != "foreign" {
		return Config{}, fmt.Errorf("default_group must be domestic or foreign")
	}
	if c.TimeoutMS < 200 || c.TimeoutMS > 5000 || c.CacheSize < 1 || c.MaxTTL == 0 || c.NegativeTTL == 0 {
		return Config{}, fmt.Errorf("invalid timeout_ms, cache_size, max_ttl or negative_ttl")
	}
	for i := range c.DomesticUpstreams {
		c.DomesticUpstreams[i] = normalizeUpstream(c.DomesticUpstreams[i])
	}
	for i := range c.ForeignUpstreams {
		c.ForeignUpstreams[i] = normalizeUpstream(c.ForeignUpstreams[i])
	}
	if c.LocalUpstream != "" {
		c.LocalUpstream = normalizeUpstream(c.LocalUpstream)
	}
	for i := range c.DN42Upstreams {
		c.DN42Upstreams[i] = normalizeUpstream(c.DN42Upstreams[i])
	}
	return c, nil
}

func normalizeUpstream(s string) string {
	if _, _, err := net.SplitHostPort(s); err == nil {
		return s
	}
	return net.JoinHostPort(s, "53")
}
