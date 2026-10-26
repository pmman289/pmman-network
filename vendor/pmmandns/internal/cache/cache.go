package cache

import (
	"github.com/miekg/dns"
	"sync"
	"time"
)

type DomesticEntry struct {
	// A/AAAA contain terminal address records used to choose the preferred
	// address family. The Answer fields preserve CNAME chains for the selected
	// family so clients receive a complete DNS answer.
	A, AAAA    []dns.RR
	AAnswer    []dns.RR
	AAAAAnswer []dns.RR
	RCode      int
	ExpireAt   time.Time
}
type Cache struct {
	mu       sync.Mutex
	domestic map[string]DomesticEntry
	foreign  map[string]*dns.Msg
	max      int
}

func New(max int) *Cache {
	return &Cache{domestic: map[string]DomesticEntry{}, foreign: map[string]*dns.Msg{}, max: max}
}
func (c *Cache) GetDomestic(k string) (DomesticEntry, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	v, ok := c.domestic[k]
	if !ok || time.Now().After(v.ExpireAt) {
		delete(c.domestic, k)
		return DomesticEntry{}, false
	}
	return cloneDomestic(v), true
}
func (c *Cache) PutDomestic(k string, v DomesticEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.domestic) >= c.max {
		for x := range c.domestic {
			delete(c.domestic, x)
			break
		}
	}
	c.domestic[k] = cloneDomestic(v)
}
func (c *Cache) GetForeign(k string) (*dns.Msg, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	v, ok := c.foreign[k]
	if !ok {
		return nil, false
	}
	return v.Copy(), true
}
func (c *Cache) PutForeign(k string, v *dns.Msg) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.foreign) >= c.max {
		for x := range c.foreign {
			delete(c.foreign, x)
			break
		}
	}
	c.foreign[k] = v.Copy()
}
func cloneDomestic(v DomesticEntry) DomesticEntry {
	return DomesticEntry{
		A:          append([]dns.RR(nil), v.A...),
		AAAA:       append([]dns.RR(nil), v.AAAA...),
		AAnswer:    append([]dns.RR(nil), v.AAnswer...),
		AAAAAnswer: append([]dns.RR(nil), v.AAAAAnswer...),
		RCode:      v.RCode,
		ExpireAt:   v.ExpireAt,
	}
}
