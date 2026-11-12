// Command zapstore publishes the latest Android release to Zapstore from CI.
//
// The stock zsp CLI can only create the NIP-C1 certificate proof through an
// interactive wizard or a running hotbox. CI has neither, so this command calls
// the zsp library with a ProofSigner backed by the release keystore. The Nostr
// event itself is signed with SIGN_WITH, as usual.
package main

import (
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"

	"github.com/nbd-wtf/go-nostr"
	"github.com/nbd-wtf/go-nostr/nip19"
	"github.com/zapstore/zsp/v2"
	"software.sslmate.com/src/go-pkcs12"
)

type keystoreProofSigner struct {
	key    crypto.PrivateKey
	cert   *x509.Certificate
	pubkey string
}

// SignProof signs the NIP-C1 message with the Android keystore key. The format
// is the one zsp uses in internal/identity: a SHA-256 digest of the message,
// signed with PKCS#1 v1.5 for RSA or ASN.1 DER for ECDSA.
func (s keystoreProofSigner) SignProof(_ context.Context, createdAt, expiry int64) (zsp.ProofSignature, error) {
	message := fmt.Sprintf(
		"Verifying at %d until %d that I control the following Nostr public key: %s",
		createdAt, expiry, s.pubkey,
	)
	digest := sha256.Sum256([]byte(message))

	var (
		signature []byte
		err       error
	)
	switch key := s.key.(type) {
	case *rsa.PrivateKey:
		signature, err = rsa.SignPKCS1v15(rand.Reader, key, crypto.SHA256, digest[:])
	case *ecdsa.PrivateKey:
		signature, err = ecdsa.SignASN1(rand.Reader, key, digest[:])
	default:
		return zsp.ProofSignature{}, fmt.Errorf("unsupported keystore key type %T", s.key)
	}
	if err != nil {
		return zsp.ProofSignature{}, fmt.Errorf("sign C1 proof: %w", err)
	}

	certHash := sha256.Sum256(s.cert.Raw)
	return zsp.ProofSignature{
		PublicKey:       s.pubkey,
		CertificateHash: hex.EncodeToString(certHash[:]),
		Signature:       signature,
	}, nil
}

// nostrPubkey returns the lowercase hex public key named by SIGN_WITH, which
// may be an nsec, a 64-character hex secret key, or a NIP-46 bunker URL.
func nostrPubkey(signWith string) (string, error) {
	switch {
	case strings.HasPrefix(signWith, "nsec1"):
		prefix, value, err := nip19.Decode(signWith)
		if err != nil || prefix != "nsec" {
			return "", fmt.Errorf("invalid nsec in SIGN_WITH")
		}
		secret, ok := value.(string)
		if !ok {
			return "", fmt.Errorf("invalid nsec in SIGN_WITH")
		}
		pubkey, err := nostr.GetPublicKey(secret)
		if err != nil {
			return "", fmt.Errorf("derive public key from SIGN_WITH: %w", err)
		}
		return pubkey, nil
	case len(signWith) == 64 && isHex(signWith):
		pubkey, err := nostr.GetPublicKey(strings.ToLower(signWith))
		if err != nil {
			return "", fmt.Errorf("derive public key from SIGN_WITH: %w", err)
		}
		return pubkey, nil
	case strings.HasPrefix(signWith, "bunker://"):
		parsed, err := url.Parse(signWith)
		if err != nil || parsed.Host == "" {
			return "", fmt.Errorf("invalid bunker URL in SIGN_WITH")
		}
		return strings.ToLower(parsed.Host), nil
	default:
		return "", fmt.Errorf("SIGN_WITH must be an nsec, a 64-character hex key, or a bunker URL")
	}
}

func isHex(value string) bool {
	_, err := hex.DecodeString(value)
	return err == nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "zapstore:", err)
		os.Exit(1)
	}
}

func run() error {
	signWith := os.Getenv("SIGN_WITH")
	if signWith == "" {
		return errors.New("SIGN_WITH is empty")
	}
	pubkey, err := nostrPubkey(signWith)
	if err != nil {
		return err
	}

	keystorePath := os.Getenv("ANDROID_KEYSTORE")
	if keystorePath == "" {
		return errors.New("ANDROID_KEYSTORE is empty")
	}
	keystoreData, err := os.ReadFile(keystorePath)
	if err != nil {
		return fmt.Errorf("read keystore: %w", err)
	}
	key, cert, _, err := pkcs12.DecodeChain(keystoreData, os.Getenv("ANDROID_KEYSTORE_PASSWORD"))
	if err != nil {
		return fmt.Errorf("open keystore (it must be PKCS#12): %w", err)
	}

	config, err := zsp.LoadConfig("zapstore.yaml")
	if err != nil {
		return err
	}

	ctx := context.Background()
	candidates, err := zsp.Fetch(ctx, config.Fetch, zsp.FetchOptions{})
	if err != nil {
		return err
	}
	if len(candidates) != 1 {
		return fmt.Errorf("expected one APK from the release, got %d", len(candidates))
	}

	result, err := zsp.Publish(ctx, config.Publish, candidates[0], zsp.PublishOptions{
		ProofSigner: keystoreProofSigner{key: key, cert: cert, pubkey: pubkey},
	})
	if err != nil {
		if errors.Is(err, zsp.ErrAlreadyPublished) {
			fmt.Println("This release is already on Zapstore.")
			return nil
		}
		return err
	}

	fmt.Printf("Published %s (version code %d).\n", candidates[0].VersionName, candidates[0].VersionCode)
	if result.ID != "" {
		fmt.Printf("Release event: %s\n", result.ID)
	}
	return nil
}
