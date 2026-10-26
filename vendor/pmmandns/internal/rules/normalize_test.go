package rules

import "testing"

func TestNormalize(t *testing.T) {
	got, err := normalize("WWW.Example.COM.")
	if err != nil || got != "www.example.com" {
		t.Fatalf("got %q, %v", got, err)
	}
}
